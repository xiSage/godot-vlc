/*
* Copyright (c) 2025 xiSage
*
* This library is free software; you can redistribute it and/or
* modify it under the terms of the GNU Lesser General Public
* License as published by the Free Software Foundation; either
* version 2.1 of the License, or (at your option) any later version.
*
* This library is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
* Lesser General Public License for more details.
*
* You should have received a copy of the GNU Lesser General Public
* License along with this library; if not, write to the Free Software
* Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301
* USA
*/

//! The acceptance test for the bundled runtime: it must decode H.264.
//!
//! This drives LibVLC directly rather than through the `vlc` command-line tool,
//! because the CLI is not what ships. The addon is a library that the extension
//! loads, so the thing worth proving is that a media player built from the
//! bundled runtime decodes a known sample, and that is what this test does.
//!
//! Driving the CLI instead was tried first and abandoned. It is a Windows GUI
//! subsystem binary that needs a console it may not be able to attach, it reads
//! and writes the invoking user's configuration, it looks for plugins beside
//! itself rather than beside the runtime, and each of those produced a failure
//! that said nothing about the shipped bytes. None of it applies to a library.
//!
//! The video path exercised here is the software one, the same callbacks
//! src/vlc_media_player/software_video.rs registers, so a regression in the
//! runtime that breaks decoding for the extension breaks this test too.
//!
//! The test needs the runtime to be loadable, which is the harness's job:
//! scripts/test.ps1 puts the staged runtime on the library search path, and
//! scripts/acceptance_test.ps1 points it at the assembled addon so that the bytes
//! under test are the ones users receive.
//!
//! The second test here is its counterpart: a media that does not exist has to
//! be reported through libvlc's error event. That event is the only report of an
//! asynchronous failure -- `libvlc_media_player_play` returns 0 for a media it
//! cannot open, and the error state is not one a caller can observe -- so a
//! runtime that stopped raising it would take the extension's `error` signal
//! down with it, and quietly.

#![cfg(test)]

use std::ffi::{CString, c_char, c_uint, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::vlc::*;

/// Where the sample is, and what it contains. The decoder's report is checked
/// against these numbers so that "a frame arrived" cannot be satisfied by some
/// other file being decoded.
const SAMPLE: &str = "test/media/h264_64x64_1s.mp4";
const SAMPLE_WIDTH: u32 = 64;
const SAMPLE_HEIGHT: u32 = 64;

/// How long the decoder is given. The sample is one second long; anything
/// approaching this bound means it is not going to happen.
const DECODE_TIMEOUT: Duration = Duration::from_secs(60);

/// The media the error test asks for, relative to the repository root.
///
/// It must not exist -- the failure is the whole point -- so nothing here creates
/// it, and the test refuses to run if something else did.
const MISSING_MEDIA: &str = "target/acceptance/this-media-does-not-exist.mp4";

/// How long the error report is given.
///
/// Nothing has to be read or connected: this runtime reported the failure about
/// 140 ms after `play` was called. The bound is far above that, and far below
/// "it hangs".
const ERROR_TIMEOUT: Duration = Duration::from_secs(15);

/// The state the video callbacks operate on.
///
/// A frame counter and the buffer the decoder writes into. It is deliberately
/// plain memory: no Godot type is involved, so the test runs without an engine.
struct DecodeProbe {
    width: u32,
    height: u32,
    buffer: Vec<u8>,
    frames: AtomicUsize,
}

impl DecodeProbe {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            buffer: Vec::new(),
            frames: AtomicUsize::new(0),
        }
    }
}

unsafe extern "C" fn lock_callback(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    unsafe {
        let probe = &mut *(opaque as *mut DecodeProbe);
        *planes = probe.buffer.as_mut_ptr() as *mut c_void;
        ptr::null_mut()
    }
}

unsafe extern "C" fn unlock_callback(
    _opaque: *mut c_void,
    _picture: *mut c_void,
    _planes: *const *mut c_void,
) {
}

unsafe extern "C" fn display_callback(opaque: *mut c_void, _picture: *mut c_void) {
    unsafe {
        let probe = &*(opaque as *const DecodeProbe);
        probe.frames.fetch_add(1, Ordering::Relaxed);
    }
}

/// Announces the decoded format and sizes the buffer the decoder will write into.
///
/// The opaque pointer is left alone, unlike in the extension, where the state
/// carries a Godot image that has to be rebuilt for each format. Here the state
/// is a plain struct that can simply resize.
unsafe extern "C" fn format_callback(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    unsafe {
        let probe = &mut *(*opaque as *mut DecodeProbe);
        probe.width = *width;
        probe.height = *height;

        // RV24 is the packed 24-bit format the extension asks for.
        chroma.copy_from(c"RV24".as_ptr(), 5);
        let pitch = *width * 3;
        *pitches = pitch;
        *lines = *height;

        probe.buffer = vec![0u8; (pitch * *height) as usize];
        1
    }
}

unsafe extern "C" fn cleanup_callback(_opaque: *mut c_void) {
    // Nothing to release: the probe is owned by the test, not by the callbacks.
}

fn sample_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SAMPLE)
}

/// The libvlc arguments for a test whose video goes to its own callbacks.
///
/// `libvlc_new` takes `const char *const *`, and an array of `&CStr` is an array
/// of *fat* pointers: each length sits right after its address, so a second
/// element would be read as a pointer to a small number and the process would die
/// dereferencing it. `CStr::as_ptr()` gives the thin pointer the C API wants. One
/// option happens to work either way, which is exactly why this is spelled out
/// rather than left as a trap for the next one.
fn quiet_options() -> [*const c_char; 1] {
    [c"--no-audio".as_ptr()]
}

/// The same, for a test that leaves libvlc to choose a video output: no window to
/// put one in.
fn headless_options() -> [*const c_char; 2] {
    [c"--no-audio".as_ptr(), c"--vout=dummy".as_ptr()]
}

/// Decodes the sample and returns the reported size and the frame count.
///
/// Returns `Err` with an explanation when LibVLC could not be set up at all,
/// so that a missing runtime reads differently from a runtime that cannot
/// decode.
fn decode_sample() -> Result<(u32, u32, usize), String> {
    let path = sample_path();
    if !path.exists() {
        return Err(format!("the sample is missing: {}", path.display()));
    }
    let path = CString::new(path.to_str().ok_or("the sample path is not valid UTF-8")?)
        .map_err(|_| "the sample path contains a NUL byte".to_string())?;

    let mut probe = Box::new(DecodeProbe::new());
    let opaque = probe.as_mut() as *mut DecodeProbe as *mut c_void;

    // No audio: a build machine has no sound device, and audio is not what this
    // test is about.
    let options = quiet_options();
    let instance = unsafe { libvlc_new(options.len() as i32, options.as_ptr()) };
    if instance.is_null() {
        return Err("libvlc_new returned NULL: the runtime could not be initialised".to_string());
    }

    let media = unsafe { libvlc_media_new_path(path.as_ptr()) };
    if media.is_null() {
        unsafe { libvlc_release(instance) };
        return Err(format!(
            "libvlc_media_new_path failed for {}",
            path.to_string_lossy()
        ));
    }

    let player = unsafe { libvlc_media_player_new(instance) };
    if player.is_null() {
        unsafe {
            libvlc_media_release(media);
            libvlc_release(instance);
        }
        return Err("libvlc_media_player_new returned NULL".to_string());
    }

    unsafe {
        libvlc_video_set_callbacks(
            player,
            Some(lock_callback),
            Some(unlock_callback),
            Some(display_callback),
            opaque,
        );
        libvlc_video_set_format_callbacks(player, Some(format_callback), Some(cleanup_callback));
        libvlc_media_player_set_media(player, media);
    }

    if unsafe { libvlc_media_player_play(player) } != 0 {
        unsafe {
            libvlc_media_player_release(player);
            libvlc_media_release(media);
            libvlc_release(instance);
        }
        return Err("libvlc_media_player_play was refused".to_string());
    }

    let deadline = Instant::now() + DECODE_TIMEOUT;
    while Instant::now() < deadline {
        if probe.frames.load(Ordering::Relaxed) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let frames = probe.frames.load(Ordering::Relaxed);
    let (width, height) = (probe.width, probe.height);

    unsafe {
        libvlc_media_player_release(player);
        libvlc_media_release(media);
        libvlc_release(instance);
    }

    Ok((width, height, frames))
}

#[test]
fn decodes_the_h264_sample() {
    let (width, height, frames) = match decode_sample() {
        Ok(result) => result,
        Err(explanation) => panic!("{explanation}"),
    };

    // The frame count is the proof: a decoder that never ran delivers none.
    assert!(
        frames > 0,
        "no frame was delivered within {DECODE_TIMEOUT:?}; the runtime loaded but did not decode"
    );

    assert_eq!(
        width, SAMPLE_WIDTH,
        "the decoder reported a width of {width}; the sample is {SAMPLE_WIDTH} wide, so something other than the expected video was decoded"
    );

    // The height is what LibVLC hands the format callback for its picture
    // buffer, which is padded rather than exactly the coded size: this sample is
    // 64x64 and LibVLC reports a height of 66. The bound is one macroblock, large
    // enough for that padding and far too small to accept a different video. The
    // reported numbers are authoritative for the buffer -- the extension sizes
    // its allocation from them -- so the padding is not something to correct for.
    assert!(
        (SAMPLE_HEIGHT..=SAMPLE_HEIGHT + 16).contains(&height),
        "the decoder reported a height of {height}; the sample is {SAMPLE_HEIGHT} high, so something other than the expected video was decoded"
    );
}

/// What the error event reported. Written from libvlc's input thread.
struct ErrorProbe {
    fired: AtomicBool,
    event_type: AtomicI32,
}

impl ErrorProbe {
    fn new() -> Self {
        Self {
            fired: AtomicBool::new(false),
            event_type: AtomicI32::new(0),
        }
    }
}

/// Records that the error event arrived, and which type it was.
///
/// Nothing else may happen here: this runs on libvlc's thread, holding the
/// player's lock, and it is not the thread that owns the probe.
unsafe extern "C" fn error_callback(event: *const libvlc_event_t, user_data: *mut c_void) {
    unsafe {
        let probe = &*(user_data as *const ErrorProbe);
        probe.event_type.store((*event).type_, Ordering::Relaxed);
        probe.fired.store(true, Ordering::Release);
    }
}

/// A media that cannot be opened has to be reported, or nothing reports it.
///
/// This is the assertion the extension's `error` signal rests on. `play` returns
/// 0 for a media it cannot open, and a failed open reaches the same
/// stopping/stopped states as a media that ended, so if the runtime stopped
/// raising this event the failure would be silent everywhere.
///
/// It drives LibVLC directly, so it pins the runtime rather than this binding's
/// use of it; `demo/tests/error_signal.gd` is the end-to-end half, where the
/// signal has to arrive in GDScript.
#[test]
fn reports_a_media_that_cannot_be_opened() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MISSING_MEDIA);
    assert!(
        !path.exists(),
        "the test needs {} to be absent, and something created it",
        path.display()
    );
    let path = CString::new(path.to_str().expect("the media path is not valid UTF-8"))
        .expect("the media path contains a NUL byte");

    // No audio: nothing is going to be decoded, and a build machine has no sound
    // device.
    let options = quiet_options();
    let instance = unsafe { libvlc_new(options.len() as i32, options.as_ptr()) };
    assert!(
        !instance.is_null(),
        "libvlc_new returned NULL: the runtime could not be initialised"
    );

    let media = unsafe { libvlc_media_new_path(path.as_ptr()) };
    assert!(!media.is_null(), "libvlc_media_new_path refused the path");

    let player = unsafe { libvlc_media_player_new(instance) };
    assert!(!player.is_null(), "libvlc_media_player_new returned NULL");

    let mut probe = Box::new(ErrorProbe::new());
    let opaque = probe.as_mut() as *mut ErrorProbe as *mut c_void;

    let attached = unsafe {
        libvlc_event_attach(
            libvlc_media_player_event_manager(player),
            libvlc_event_e_libvlc_MediaPlayerEncounteredError as libvlc_event_type_t,
            Some(error_callback),
            opaque,
        )
    };
    assert_eq!(attached, 0, "the error event could not be attached");

    unsafe { libvlc_media_player_set_media(player, media) };

    // A media that cannot be opened is accepted here: the path is not touched
    // until the input thread runs. That is the reason this event is needed, so it
    // is asserted rather than assumed -- a runtime that started reporting the
    // failure from `play` would make the premise of this test false.
    let started = unsafe { libvlc_media_player_play(player) };
    assert_eq!(
        started, 0,
        "play returned {started} for a media that cannot be opened; this test expects the failure to arrive asynchronously"
    );

    let deadline = Instant::now() + ERROR_TIMEOUT;
    while Instant::now() < deadline && !probe.fired.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let fired = probe.fired.load(Ordering::Acquire);
    let reported = probe.event_type.load(Ordering::Relaxed);

    unsafe {
        libvlc_media_player_release(player);
        libvlc_media_release(media);
        libvlc_release(instance);
    }

    assert!(
        fired,
        "no event arrived within {ERROR_TIMEOUT:?} for a media that does not exist; a caller would see the failure as nothing happening"
    );
    assert_eq!(
        reported, libvlc_event_e_libvlc_MediaPlayerEncounteredError as libvlc_event_type_t,
        "the event that arrived was of type {reported}, not the error event"
    );
}

/// The A to B loop the loop tests ask for: the first 400 ms of the sample.
///
/// A short loop is what a game wants it for, and it is also where the runtime's
/// own behaviour shows: measured against the pinned runtime, a B below about
/// 400 ms loops at about 400 ms whatever was asked for, so asking for 400 is
/// asking for what actually happens.
const LOOP_B_MS: i64 = 400;

/// How long a wrap is watched for, and how long its absence is watched for.
///
/// The sample is one second long and the loop closes every ~400 ms, so three
/// seconds holds several wraps -- and, for a player that is no longer looping,
/// at most the single backwards jump at the end of the media.
const LOOP_OBSERVATION: Duration = Duration::from_secs(3);

/// How long playback is given to start or stop. The sample is one second long.
const PLAYBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The sample, the instance, and a player for it, released when this drops.
///
/// The media is not handed to the player until [Sample::attach_media] does it,
/// because that is what creates the input a loop belongs to -- which is the
/// thing these tests are about.
struct Sample {
    instance: *mut libvlc_instance_t,
    media: *mut libvlc_media_t,
    player: *mut libvlc_media_player_t,
}

impl Sample {
    fn new() -> Self {
        let path = sample_path();
        let path = CString::new(path.to_str().expect("the sample path is not valid UTF-8"))
            .expect("the sample path contains a NUL byte");

        // No audio and no window: neither is what this is about, and a build
        // machine has no sound device.
        let options = headless_options();
        let instance = unsafe { libvlc_new(options.len() as i32, options.as_ptr()) };
        assert!(
            !instance.is_null(),
            "libvlc_new returned NULL: the runtime could not be initialised"
        );
        let media = unsafe { libvlc_media_new_path(path.as_ptr()) };
        assert!(!media.is_null(), "libvlc_media_new_path refused the sample");
        let player = unsafe { libvlc_media_player_new(instance) };
        assert!(!player.is_null(), "libvlc_media_player_new returned NULL");
        Self {
            instance,
            media,
            player,
        }
    }

    fn attach_media(&self) {
        unsafe { libvlc_media_player_set_media(self.player, self.media) };
    }

    fn set_loop(&self, a_ms: i64, b_ms: i64) -> i32 {
        unsafe { libvlc_media_player_set_abloop_time(self.player, a_ms, b_ms) }
    }

    fn play(&self) -> i32 {
        unsafe { libvlc_media_player_play(self.player) }
    }

    fn state(&self) -> i32 {
        unsafe { libvlc_media_player_get_state(self.player) }
    }

    fn time(&self) -> i64 {
        unsafe { libvlc_media_player_get_time(self.player) }
    }

    /// What libvlc reports about the loop, as `(status, a_time, b_time)`.
    ///
    /// Only the outputs the status covers are returned. The C API writes all
    /// four of them either way, and when no loop is set two of them are
    /// uninitialised stack memory of libvlc's own frame rather than a value.
    fn ab_loop(&self) -> (i32, i64, i64) {
        let mut a_time: i64 = -1;
        let mut a_pos: f64 = -1.0;
        let mut b_time: i64 = -1;
        let mut b_pos: f64 = -1.0;
        let status = unsafe {
            libvlc_media_player_get_abloop(
                self.player,
                &mut a_time,
                &mut a_pos,
                &mut b_time,
                &mut b_pos,
            )
        };
        let has_a = status >= libvlc_abloop_t_libvlc_abloop_a;
        let has_b = status >= libvlc_abloop_t_libvlc_abloop_b;
        (
            status,
            if has_a { a_time } else { -1 },
            if has_b { b_time } else { -1 },
        )
    }

    /// Waits for the state libvlc reports to become `want`.
    fn wait_for_state(&self, want: i32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.state() == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Watches playback for `duration`, returning every time it moved backwards
    /// and the states seen along the way.
    ///
    /// A backwards move of more than a tenth of a second is what a loop looks
    /// like from outside; the end of a media that is not looping is exactly one
    /// of them, which is what tells the two apart.
    fn observe(&self, duration: Duration) -> (Vec<(i64, i64)>, Vec<i32>) {
        let mut drops = Vec::new();
        let mut states = Vec::new();
        let mut previous = self.time();
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            let state = self.state();
            let time = self.time();
            states.push(state);
            if time < previous - 100 {
                drops.push((previous, time));
            }
            previous = time;
        }
        (drops, states)
    }
}

impl Drop for Sample {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_player_release(self.player);
            libvlc_media_release(self.media);
            libvlc_release(self.instance);
        }
    }
}

/// Starts the sample and waits for libvlc to report it playing.
fn play_until_playing(sample: &Sample) {
    assert_eq!(sample.play(), 0, "play was refused");
    assert!(
        sample.wait_for_state(libvlc_state_t_libvlc_Playing, PLAYBACK_TIMEOUT),
        "playback never started within {PLAYBACK_TIMEOUT:?}"
    );
}

/// A loop set before playback runs, and describes itself accurately.
///
/// This is what the extension's A to B API rests on: the input wraps and stays in
/// playback while doing it, and `get_abloop` says which loop is running. It also
/// pins the two things a caller cannot guess -- that a loop needs a media to
/// belong to, and that it can be set before playback starts.
#[test]
fn loops_between_two_times() {
    let sample = Sample::new();

    // Before a media is assigned there is no input, and libvlc answers with an
    // error rather than remembering the request for later.
    assert_eq!(
        sample.set_loop(0, LOOP_B_MS),
        -1,
        "a loop was accepted for a player with no media; there is nothing for it to belong to"
    );

    sample.attach_media();

    // An assigned media is enough: the input exists before playback does, which
    // is what lets a script that knows its clips set the loop up front.
    assert_eq!(
        sample.set_loop(0, LOOP_B_MS),
        0,
        "libvlc refused a loop between 0 and {LOOP_B_MS} ms"
    );

    play_until_playing(&sample);

    let (status, a_time, b_time) = sample.ab_loop();
    assert_eq!(
        status, libvlc_abloop_t_libvlc_abloop_b,
        "get_abloop reported status {status}, not the complete-loop status"
    );
    assert_eq!(a_time, 0, "get_abloop reported a wrong A point");
    assert_eq!(b_time, LOOP_B_MS, "get_abloop reported a wrong B point");

    let (drops, states) = sample.observe(LOOP_OBSERVATION);
    assert!(
        drops.len() >= 2,
        "a {LOOP_B_MS} ms loop did not wrap more than once in {LOOP_OBSERVATION:?}: the backwards moves were {drops:?}"
    );
    // The sample is one second long and the loop closes every ~400 ms, so a loop
    // that is working never reaches the end of the media and never leaves
    // playback. Buffering is allowed through: a wrap is a seek, and a loaded
    // machine may report it.
    assert!(
        states
            .iter()
            .all(|state| *state != libvlc_state_t_libvlc_Stopping
                && *state != libvlc_state_t_libvlc_Stopped
                && *state != libvlc_state_t_libvlc_Error),
        "playback left the running states while looping: {states:?}"
    );
}

/// The loop goes away with the input, and nothing brings it back.
///
/// A script that stops playback and starts it again has lost its loop, and the
/// only notice is `get_abloop` reporting nothing. That is worth pinning: it is
/// the part most likely to be "fixed" by someone who reads the API as
/// per-player, which it is not.
#[test]
fn a_loop_does_not_survive_a_stop() {
    let sample = Sample::new();
    sample.attach_media();
    assert_eq!(
        sample.set_loop(0, LOOP_B_MS),
        0,
        "libvlc refused a loop between 0 and {LOOP_B_MS} ms"
    );
    play_until_playing(&sample);

    // It has to be running first, or losing it would prove nothing.
    let (before, _) = sample.observe(Duration::from_millis(1200));
    assert!(
        !before.is_empty(),
        "the loop was not wrapping before the stop, so this test would pass for the wrong reason"
    );

    assert_eq!(
        unsafe { libvlc_media_player_stop_async(sample.player) },
        0,
        "the stop was refused"
    );
    assert!(
        sample.wait_for_state(libvlc_state_t_libvlc_Stopped, PLAYBACK_TIMEOUT),
        "playback never stopped within {PLAYBACK_TIMEOUT:?}"
    );

    play_until_playing(&sample);

    let (status, _, _) = sample.ab_loop();
    assert_eq!(
        status, libvlc_abloop_t_libvlc_abloop_none,
        "the loop outlived the input it was set on"
    );

    // Nothing loops it now, so the only backwards move left is the media's own
    // end.
    let (after, _) = sample.observe(LOOP_OBSERVATION);
    assert!(
        after.len() <= 1,
        "the loop outlived the stop: the backwards moves were {after:?}"
    );
}
