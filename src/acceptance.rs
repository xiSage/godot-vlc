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
//!
//! The rest is one test per thing the extension promises on top of libvlc: the A
//! to B loop (a loop set in milliseconds, one set as positions, and that it does
//! not outlive the input it was set on), per-media options (read when the input is
//! created, and not read after that), the subtitle entry points (`slaves_add`
//! before playback, `add_slave` only with an input, the delay belonging to the
//! input while the text scale belongs to the player), and the track struct: which
//! member of its union is read, what each of the two samples reports, and what a
//! subtitle track declares.

#![cfg(test)]
// The bindgen-generated enum types differ per target -- `libvlc_state_t` and
// `libvlc_abloop_t` come out as `c_int` on Windows and as `u32` on the Linux and
// Android targets -- so the casts to `i32` below are required by some targets and
// redundant on the others. `vlc_media_player.rs` carries the same cast, and the
// same allow, for the `STATE_*` and `ABLOOP_*` constants it exports.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::vlc::*;
use crate::vlc_track::{TrackInfo, read_info};

/// Where the sample is, and what it contains. The decoder's report is checked
/// against these numbers so that "a frame arrived" cannot be satisfied by some
/// other file being decoded.
const SAMPLE: &str = "test/media/h264_64x64_1s.mp4";
const SAMPLE_WIDTH: u32 = 64;
const SAMPLE_HEIGHT: u32 = 64;
/// The sample's pixel aspect ratio and frame rate, from `ffprobe` over the checked-in
/// file: square pixels, ten frames a second.
const SAMPLE_SAR: (u32, u32) = (1, 1);
const SAMPLE_FRAME_RATE: (u32, u32) = (10, 1);

/// The second sample, for the track fields the first one cannot show.
///
/// It is the demo's media, and nothing else in the repository writes down what is
/// in it; these numbers come from `ffprobe` over the checked-in file. It is here
/// because a track's pixel aspect ratio and its audio member need a media that
/// has a non-square one and an audio track at all.
const SECOND_SAMPLE: &str = "demo/test.mp4";
/// 854 by 480, reported from the *visible* size.
const SECOND_SAMPLE_WIDTH: u32 = 854;
const SECOND_SAMPLE_HEIGHT: u32 = 480;
/// Very nearly square and not exactly square, which is the point: a binding that
/// read a default of 1:1, or that read the wrong union member, still has to fail.
const SECOND_SAMPLE_SAR_NUM: u32 = 1280;
const SECOND_SAMPLE_SAR_DEN: u32 = 1281;
/// AAC stereo at 48 kHz.
const SECOND_SAMPLE_CHANNELS: u32 = 2;
const SECOND_SAMPLE_RATE: u32 = 48_000;

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

fn path_of(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn sample_path() -> PathBuf {
    path_of(SAMPLE)
}

/// Reads every track in a tracklist, then releases the list.
///
/// The reads happen while the list is alive and copy what they find, so no track
/// has to be held past it. A null list is what libvlc returns when there is no
/// track of that category, and it reads as empty rather than as an error: no
/// track and a refused query are the same thing to a caller that asked for a
/// category.
fn collect_tracks(list: *mut libvlc_media_tracklist_t) -> Vec<TrackInfo> {
    if list.is_null() {
        return Vec::new();
    }
    let count = unsafe { libvlc_media_tracklist_count(list) };
    let mut tracks = Vec::with_capacity(count);
    for index in 0..count {
        let track = unsafe { libvlc_media_tracklist_at(list, index) };
        tracks.push(unsafe { read_info(track) });
    }
    unsafe { libvlc_media_tracklist_delete(list) };
    tracks
}

/// Polls a tracklist until its video track reports a size, or `timeout` passes.
///
/// A track is created by an input from the format the demuxer declared, and
/// libvlc has no picture size at that point: the size comes later, with the
/// decoder's format report, and it comes as a *new* track -- libvlc publishes a
/// new tracklist rather than changing the track it already handed out, so the
/// first snapshot keeps its zeroes. Anything that wants the size has to ask
/// again, and this is what asking again looks like.
fn wait_for_known_size<F>(mut read: F, timeout: Duration) -> Vec<TrackInfo>
where
    F: FnMut() -> Vec<TrackInfo>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let tracks = read();
        let known = tracks
            .iter()
            .any(|track| track.video.as_ref().is_some_and(|video| video.width != 0));
        if known || Instant::now() >= deadline {
            return tracks;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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

/// The most the reported time may rise between two reads and still be the clock
/// running.
///
/// The reads are 10 ms apart, so an ordinary read moves by about that much; a value
/// more than a tenth of a second above the one before it was not arrived at by
/// playing, and a backwards move that starts from such a value is not a wrap. See
/// [Sample::watch].
const RISE_PER_POLL_MS: i64 = 100;

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
        Self::from_path(sample_path())
    }

    /// The same, for a media that is not the sample.
    fn from_path(path: PathBuf) -> Self {
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

    fn set_loop_by_position(&self, a_pos: f64, b_pos: f64) -> i32 {
        unsafe { libvlc_media_player_set_abloop_position(self.player, a_pos, b_pos) }
    }

    fn play(&self) -> i32 {
        unsafe { libvlc_media_player_play(self.player) }
    }

    fn state(&self) -> i32 {
        unsafe { libvlc_media_player_get_state(self.player) as i32 }
    }

    fn time(&self) -> i64 {
        unsafe { libvlc_media_player_get_time(self.player) }
    }

    /// What libvlc reports about the loop, as
    /// `(status, a_time, a_pos, b_time, b_pos)`.
    ///
    /// Only the outputs the status covers are returned. The C API writes all
    /// four of them either way, and when no loop is set two of them are
    /// uninitialised stack memory of libvlc's own frame rather than a value.
    fn ab_loop(&self) -> (i32, i64, f64, i64, f64) {
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
            status as i32,
            if has_a { a_time } else { -1 },
            if has_a { a_pos } else { -1.0 },
            if has_b { b_time } else { -1 },
            if has_b { b_pos } else { -1.0 },
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

    /// Watches playback for `timeout`, returning every backwards move it saw and
    /// the states seen along the way.
    fn observe(&self, timeout: Duration) -> (Vec<(i64, i64)>, Vec<i32>) {
        self.watch(None, timeout)
    }

    /// Watches playback until it moves backwards once, or `timeout` passes.
    ///
    /// The stop test needs the loop to have wrapped before the stop, and how much
    /// wall-clock time that takes is not fixed: a loaded machine gets through the
    /// same 400 ms of media more slowly than an idle one, which is how a wrap came
    /// to be missed inside a 1200 ms window.
    fn observe_until_a_wrap(&self, timeout: Duration) -> Vec<(i64, i64)> {
        self.watch(Some(1), timeout).0
    }

    /// The shared body of the two above: watches until `wanted` backwards moves
    /// have been seen, or until `timeout` passes.
    ///
    /// A backwards move is how a loop shows up from outside, and the end of a media
    /// that is not looping is one of them too, which is what tells the two apart. It
    /// is counted when the time comes back down by more than a tenth of a second
    /// from a value the timeline had *risen into* -- one poll's worth of rise, which
    /// is what a running clock looks like. That second condition is measured: after
    /// a restart the reported time sits at `0`, then reads `200` for about 70 ms, and
    /// only then runs (`96`, `106`, `117`, ...), and the end of that `200` is a
    /// backwards move of its own that nothing moved for.
    fn watch(&self, wanted: Option<usize>, timeout: Duration) -> (Vec<(i64, i64)>, Vec<i32>) {
        let mut drops = Vec::new();
        let mut states = Vec::new();
        let mut before_previous = self.time();
        let mut previous = before_previous;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && wanted.is_none_or(|wanted| drops.len() < wanted) {
            std::thread::sleep(Duration::from_millis(10));
            let state = self.state();
            let time = self.time();
            states.push(state);
            let rose_into_it =
                previous > before_previous && previous - before_previous <= RISE_PER_POLL_MS;
            if rose_into_it && time < previous - 100 {
                drops.push((previous, time));
            }
            before_previous = previous;
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
        sample.wait_for_state(libvlc_state_t_libvlc_Playing as i32, PLAYBACK_TIMEOUT),
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

    let (status, a_time, _, b_time, _) = sample.ab_loop();
    assert_eq!(
        status, libvlc_abloop_t_libvlc_abloop_b as i32,
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
            .all(|state| *state != libvlc_state_t_libvlc_Stopping as i32
                && *state != libvlc_state_t_libvlc_Stopped as i32
                && *state != libvlc_state_t_libvlc_Error as i32),
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
    let before = sample.observe_until_a_wrap(LOOP_OBSERVATION);
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
        sample.wait_for_state(libvlc_state_t_libvlc_Stopped as i32, PLAYBACK_TIMEOUT),
        "playback never stopped within {PLAYBACK_TIMEOUT:?}"
    );

    play_until_playing(&sample);

    let (status, ..) = sample.ab_loop();
    assert_eq!(
        status, libvlc_abloop_t_libvlc_abloop_none as i32,
        "the loop outlived the input it was set on"
    );

    // Nothing loops it now, so the only backwards move left is the media's own
    // end. A loop that survived wraps every ~400 ms and makes a backwards move per
    // wrap, so six or so in this window rather than one.
    let (after, _) = sample.observe(LOOP_OBSERVATION);
    assert!(
        after.len() <= 1,
        "the loop outlived the stop: the backwards moves were {after:?}"
    );
}

/// The position entry point sets the same loop, and reports itself in positions.
///
/// Worth its own test because it is the entry point that can be got wrong
/// quietly: the times come back as `-1` afterwards, and a binding that passed
/// them on as the loop's times would be inventing values.
#[test]
fn loops_between_two_positions() {
    let sample = Sample::new();
    sample.attach_media();
    assert_eq!(
        sample.set_loop_by_position(0.0, 0.4),
        0,
        "libvlc refused a loop between positions 0.0 and 0.4"
    );
    play_until_playing(&sample);

    let (status, a_time, a_pos, b_time, b_pos) = sample.ab_loop();
    assert_eq!(
        status, libvlc_abloop_t_libvlc_abloop_b as i32,
        "get_abloop reported status {status}, not the complete-loop status"
    );
    assert_eq!(
        a_time, -1,
        "a loop set by position reported a time of {a_time}; there is none to report"
    );
    assert_eq!(
        b_time, -1,
        "a loop set by position reported a time of {b_time}; there is none to report"
    );
    assert_eq!(a_pos, 0.0, "get_abloop reported a wrong A position");
    assert_eq!(b_pos, 0.4, "get_abloop reported a wrong B position");

    let (drops, _) = sample.observe(LOOP_OBSERVATION);
    assert!(
        drops.len() >= 2,
        "a loop over positions 0.0-0.4 did not wrap more than once in {LOOP_OBSERVATION:?}: the backwards moves were {drops:?}"
    );
}

/// The per-media start time the option tests ask for, in seconds.
///
/// `start-time` is a float option -- the unit is seconds, not milliseconds -- and
/// the input seeks its demux to it when it starts.
const START_TIME_SECS: f64 = 0.5;

/// What the sample reports as its length when nothing was skipped, and when the
/// start time above was.
///
/// Measured against the pinned runtime: `:start-time=0.5` on this one-second sample
/// makes `get_length()` report 500 ms rather than 1000.
const FULL_LENGTH_MIN_MS: i64 = 900;
const SHORTENED_LENGTH_MAX_MS: i64 = 700;

/// How much less of the sample a playback that started at `:start-time=0.5` has to
/// play.
///
/// Measured: the sample plays for about 1.1 s from the beginning and about 0.6 s
/// from the offset. The margin is far below that difference, so it cannot be
/// satisfied by the shortened length alone -- a playback that skipped nothing would
/// still have to decode the whole sample.
const SKIPPED_PLAYBACK_MARGIN: Duration = Duration::from_millis(250);

/// The subtitle the slave tests attach: two cues over the sample's one second, in the
/// simplest format the subtitle demux recognises.
const SAMPLE_SUBTITLE: &str =
    "1\n00:00:00,000 --> 00:00:00,500\nAAAA\n\n2\n00:00:00,500 --> 00:00:01,000\nBBBB\n";

/// Where that subtitle is written: a generated file, next to the media the tests ask
/// for and for the same reason.
const SUBTITLE_FILE: &str = "target/acceptance/subtitle.srt";

/// Writes the subtitle fixture and returns a `file://` MRL for it.
///
/// A slave takes a URI, so this is the form a subtitle on disk is handed over in --
/// and the one VobSub pairs need, since that demux finds its second file by rewriting
/// the path it was given. Note that a path is not an MRL: a Windows path handed over
/// as one would be split at its drive colon, which is why the `file:///` is built
/// here. Nothing in the test path needs percent-encoding; a path that did (a space, a
/// `#`, a `%`) would, and `VLCSubtitle.load_from_file` is what does that for callers.
fn subtitle_mrl() -> CString {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SUBTITLE_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the subtitle directory could not be created");
    }
    std::fs::write(&path, SAMPLE_SUBTITLE).expect("the subtitle fixture could not be written");

    let path = path
        .to_str()
        .expect("the subtitle path is not valid UTF-8")
        .replace('\\', "/");
    let mrl = if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    };
    CString::new(mrl).expect("the subtitle MRL contains a NUL byte")
}

impl Sample {
    /// Adds a per-media option to the media.
    ///
    /// Where this is called in a test is the point: libvlc reads the options when
    /// the input is created, and `attach_media` is what does that.
    fn add_option(&self, option: &str) {
        let option = CString::new(option).expect("the option contains a NUL byte");
        unsafe { libvlc_media_add_option(self.media, option.as_ptr()) };
    }

    /// Adds a subtitle to the media descriptor, before it is attached.
    fn add_slave_to_media(&self, uri: &CStr, priority: c_uint) -> i32 {
        unsafe {
            libvlc_media_slaves_add(
                self.media,
                libvlc_media_slave_type_t_libvlc_media_slave_type_subtitle,
                priority,
                uri.as_ptr(),
            )
        }
    }

    /// Adds a subtitle to the player that exists, if one does.
    fn add_slave_to_player(&self, uri: &CStr, select: bool) -> i32 {
        unsafe {
            libvlc_media_player_add_slave(
                self.player,
                libvlc_media_slave_type_t_libvlc_media_slave_type_subtitle,
                uri.as_ptr(),
                select,
            )
        }
    }

    /// How many text tracks the player reports right now.
    fn text_tracks(&self) -> usize {
        unsafe {
            let list = libvlc_media_player_get_tracklist(
                self.player,
                libvlc_track_type_t_libvlc_track_text,
                false,
            );
            if list.is_null() {
                return 0;
            }
            let count = libvlc_media_tracklist_count(list);
            libvlc_media_tracklist_delete(list);
            count
        }
    }

    /// Waits for a text track to appear.
    fn wait_for_text_track(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.text_tracks() > 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Every track of one type the media descriptor reports, read the way the
    /// binding reads them.
    fn media_tracks(&self, track_type: libvlc_track_type_t) -> Vec<TrackInfo> {
        unsafe { collect_tracks(libvlc_media_get_tracklist(self.media, track_type)) }
    }

    /// The same, from the player. `selected` filters to the selected tracks only.
    fn player_tracks(&self, track_type: libvlc_track_type_t, selected: bool) -> Vec<TrackInfo> {
        unsafe {
            collect_tracks(libvlc_media_player_get_tracklist(
                self.player,
                track_type,
                selected,
            ))
        }
    }

    /// Waits until the player reports at least one track of this type.
    ///
    /// A track is created by an input, so it is not there the moment `play`
    /// returns; the first poll that answers is the one a caller would see after
    /// its first event.
    fn wait_for_track(&self, track_type: libvlc_track_type_t, timeout: Duration) -> Vec<TrackInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let tracks = self.player_tracks(track_type, false);
            if !tracks.is_empty() || Instant::now() >= deadline {
                return tracks;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn set_spu_delay(&self, delay_us: i64) -> i32 {
        unsafe { libvlc_video_set_spu_delay(self.player, delay_us) }
    }

    fn spu_delay(&self) -> i64 {
        unsafe { libvlc_video_get_spu_delay(self.player) }
    }

    fn set_spu_text_scale(&self, scale: f32) {
        unsafe { libvlc_video_set_spu_text_scale(self.player, scale) }
    }

    fn spu_text_scale(&self) -> f32 {
        unsafe { libvlc_video_get_spu_text_scale(self.player) }
    }

    fn length(&self) -> i64 {
        unsafe { libvlc_media_player_get_length(self.player) }
    }

    /// Waits for playback to stop, and says how long it ran.
    ///
    /// Call it as soon as playback is reported playing; the tests below use it to
    /// measure how much of the sample was played.
    fn play_out(&self, timeout: Duration) -> Option<Duration> {
        let begin = Instant::now();
        self.wait_for_state(libvlc_state_t_libvlc_Stopped as i32, timeout)
            .then(|| begin.elapsed())
    }
}

/// A per-media option is read when the media is attached, and it takes effect.
///
/// This is what the extension's `VLCMedia.add_option` rests on. The option is added
/// before the media is handed to the player -- the only moment it is read -- and the
/// two things measured here are what "read" means: the length reports the sample
/// minus the offset, and playback really is that much shorter rather than the
/// bookkeeping alone having moved.
///
/// A control playback without the option is measured in the same run: a shortened
/// length would otherwise prove nothing on its own.
#[test]
fn a_per_media_option_is_read_when_the_media_is_attached() {
    let control = Sample::new();
    control.attach_media();
    play_until_playing(&control);
    let control_length = control.length();
    let control_played = control.play_out(PLAYBACK_TIMEOUT);

    let sample = Sample::new();
    sample.add_option(&format!(":start-time={START_TIME_SECS}"));
    sample.attach_media();
    play_until_playing(&sample);
    let started_length = sample.length();
    let skipped_played = sample.play_out(PLAYBACK_TIMEOUT);

    println!(
        "per-media option: without it the sample reports {control_length} ms and plays for \
         {control_played:?}; with :start-time={START_TIME_SECS} it reports {started_length} ms and \
         plays for {skipped_played:?}"
    );

    assert!(
        control_length >= FULL_LENGTH_MIN_MS,
        "the control playback reports {control_length} ms; this test needs the whole sample to compare against"
    );
    assert!(
        started_length <= SHORTENED_LENGTH_MAX_MS,
        ":start-time={START_TIME_SECS} was added before the media was attached, and the length is still {started_length} ms: the option was not read"
    );

    let (control_played, skipped_played) = match (control_played, skipped_played) {
        (Some(control_played), Some(skipped_played)) => (control_played, skipped_played),
        (control_played, skipped_played) => panic!(
            "playback did not stop within {PLAYBACK_TIMEOUT:?}: the control ran for {control_played:?} and the one with the option for {skipped_played:?}"
        ),
    };
    assert!(
        skipped_played + SKIPPED_PLAYBACK_MARGIN <= control_played,
        "a playback that asked to start {START_TIME_SECS} s in ran for {skipped_played:?} against the control's {control_played:?}: it decoded the whole sample instead of starting at the offset"
    );
}

/// The moment the option arrives decides whether it is read.
///
/// Attaching the media is what creates the input, so an option added afterwards is
/// not read for the playback it arrives during -- the caller sees it silently do
/// nothing, which is exactly the mistake the extension's documentation has to warn
/// about. That it is not lost is the other half: the next input, after a stop, reads
/// it.
#[test]
fn a_per_media_option_added_after_the_media_is_attached_waits_for_the_next_input() {
    let sample = Sample::new();
    sample.attach_media();

    sample.add_option(&format!(":start-time={START_TIME_SECS}"));
    play_until_playing(&sample);
    let during_length = sample.length();

    assert!(
        during_length >= FULL_LENGTH_MIN_MS,
        "an option added after the media was attached was read anyway: the length is {during_length} ms"
    );

    assert_eq!(
        unsafe { libvlc_media_player_stop_async(sample.player) },
        0,
        "the stop was refused"
    );
    assert!(
        sample.wait_for_state(libvlc_state_t_libvlc_Stopped as i32, PLAYBACK_TIMEOUT),
        "playback never stopped within {PLAYBACK_TIMEOUT:?}"
    );

    play_until_playing(&sample);
    let rebuilt_length = sample.length();

    println!(
        "per-media option added after the attach: {during_length} ms of length for that playback, \
         {rebuilt_length} ms once the input was rebuilt"
    );

    assert!(
        rebuilt_length <= SHORTENED_LENGTH_MAX_MS,
        "the option was not read by the rebuilt input either: the length is {rebuilt_length} ms"
    );
}

/// A subtitle attached to the media before it is loaded becomes a track.
///
/// This is the half of the subtitle API that works before playback: `slaves_add`
/// writes the media's slave list, and the input reads that list when it is created --
/// which assigning the media is what does. There is no event for a slave, so the
/// evidence is the track list: the subtitle demux adds its track as soon as it opens.
#[test]
fn a_subtitle_attached_before_playback_becomes_a_track() {
    let sample = Sample::new();
    let subtitle = subtitle_mrl();

    assert_eq!(
        sample.add_slave_to_media(&subtitle, 4),
        0,
        "libvlc refused a subtitle for the media"
    );
    assert_eq!(
        sample.text_tracks(),
        0,
        "a text track exists before anything was played, so this test would pass for the wrong reason"
    );

    sample.attach_media();
    play_until_playing(&sample);

    assert!(
        sample.wait_for_text_track(Duration::from_secs(5)),
        "the subtitle never became a track; the media's slave list is read when the input is created"
    );
}

/// The player's own entry point needs a playback, and is the only one that has one.
///
/// The two entry points do not overlap: this one answers `VLC_EGENERIC` -- which is
/// `INT_MIN`, not the `-1` its header documents -- while there is no input, and `0`
/// once there is one. Anything added before the media is assigned has to go through
/// the media instead.
#[test]
fn a_subtitle_added_to_a_player_without_an_input_is_refused() {
    let sample = Sample::new();
    let subtitle = subtitle_mrl();

    assert_eq!(
        sample.add_slave_to_player(&subtitle, true),
        i32::MIN,
        "adding a subtitle to a player with no input did not answer VLC_EGENERIC"
    );
    assert_eq!(
        sample.text_tracks(),
        0,
        "a text track appeared for a subtitle that was refused"
    );

    sample.attach_media();
    play_until_playing(&sample);

    assert_eq!(
        sample.add_slave_to_player(&subtitle, true),
        0,
        "adding a subtitle to a playing player was refused"
    );
    assert!(
        sample.wait_for_text_track(Duration::from_secs(5)),
        "a subtitle added while playing never became a track"
    );
}

/// The subtitle delay belongs to the input and the text scale to the player.
///
/// The difference is not cosmetic, it is what a caller has to know: the delay is
/// accepted and dropped while there is no input -- with `0` returned either way, so
/// nothing reports it -- and it is gone after a stop, while the scale can be set
/// before playback and outlives both a stop and a changed media.
#[test]
fn the_subtitle_delay_dies_with_the_input_and_the_text_scale_does_not() {
    let sample = Sample::new();

    assert_eq!(
        sample.set_spu_delay(250_000),
        0,
        "the delay was refused for a player with no input; the header says it cannot fail"
    );
    assert_eq!(
        sample.spu_delay(),
        0,
        "a delay set before there was an input was kept somewhere"
    );
    sample.set_spu_text_scale(2.0);
    assert!(
        (sample.spu_text_scale() - 2.0).abs() < 0.001,
        "the text scale did not take effect before playback, although it belongs to the player"
    );

    sample.attach_media();
    play_until_playing(&sample);

    assert_eq!(
        sample.set_spu_delay(250_000),
        0,
        "the delay was refused while playing"
    );
    assert_eq!(
        sample.spu_delay(),
        250_000,
        "the delay did not survive the call while playing"
    );
    assert!(
        (sample.spu_text_scale() - 2.0).abs() < 0.001,
        "the text scale went away when playback started"
    );

    assert_eq!(
        unsafe { libvlc_media_player_stop_async(sample.player) },
        0,
        "the stop was refused"
    );
    assert!(
        sample.wait_for_state(libvlc_state_t_libvlc_Stopped as i32, PLAYBACK_TIMEOUT),
        "playback never stopped within {PLAYBACK_TIMEOUT:?}"
    );

    assert_eq!(
        sample.spu_delay(),
        0,
        "the delay outlived the input it was set on"
    );
    assert!(
        (sample.spu_text_scale() - 2.0).abs() < 0.001,
        "the text scale went away with the input, although it belongs to the player"
    );
}

/// A track's geometry is the file's numbers, or all six fields are zero.
///
/// Nothing read the video member of the union before, so what this pins is that the
/// member is there, that the binding reads the member its type names, and that the
/// sample's declared layout is the default one. It also pins the number the
/// documentation has to warn about, and that number turned out not to be a property of
/// the file: measured, this sample answers `0x0`, `sar 0/0` and a frame rate of `0/0`
/// on a Windows machine -- and its real `64x64`, `1:1` and `10/1` on the Linux CI
/// runner, and once on the Windows one too, which is what makes the assertion below the
/// only one a caller can rest on. Which of the two arrives is whatever the demuxer or
/// the decoder had written when the track was published; a third answer would be a
/// value libvlc invented. `demo/test.mp4` reports `854x480` and `1280:1281` on both
/// platforms, and the test below asserts that. Either way the size of the frames this
/// extension hands over comes from the video callbacks; a track is metadata.
#[test]
fn a_video_track_reports_the_sample_it_was_built_from() {
    let track = video_track_of(&Sample::new());
    assert_sample_geometry(&track, "the sample's video track");

    let video = track.video.as_ref().expect("checked by video_track_of");
    assert_eq!(
        (video.orientation, video.projection, video.multiview),
        (
            libvlc_video_orient_t_libvlc_video_orient_top_left as i32,
            libvlc_video_projection_t_libvlc_video_projection_rectangular as i32,
            libvlc_video_multiview_t_libvlc_video_multiview_2d as i32,
        ),
        "the sample declares no rotation, no projection and no stereoscopy"
    );

    // Printed rather than asserted: whether an H.264 profile and level reach the track
    // is the runtime's business, and the numbers are worth having in the log.
    println!(
        "the sample's video track: {}x{}, sar {}/{}, frame rate {}/{}, orientation {}, \
         projection {}, multiview {}, pose {}/{}/{}/{}, profile {}, level {}, fourcc {:#x}",
        video.width,
        video.height,
        video.sar_num,
        video.sar_den,
        video.frame_rate_num,
        video.frame_rate_den,
        video.orientation,
        video.projection,
        video.multiview,
        video.pose_yaw,
        video.pose_pitch,
        video.pose_roll,
        video.pose_field_of_view,
        track.profile,
        track.level,
        track.original_fourcc,
    );
}

/// The single video track a player reports, with the member checks every reader of a
/// track depends on.
///
/// It starts the playback itself: a track is created by an input, and the input is
/// created when the media is handed to the player.
fn video_track_of(sample: &Sample) -> TrackInfo {
    sample.attach_media();
    play_until_playing(sample);

    let tracks = sample.wait_for_track(
        libvlc_track_type_t_libvlc_track_video,
        Duration::from_secs(5),
    );
    assert_eq!(
        tracks.len(),
        1,
        "the sample has one video track and the player did not report it"
    );
    let track = tracks.into_iter().next().expect("just checked");
    assert_eq!(
        track.i_type, libvlc_track_type_t_libvlc_track_video as i32,
        "the video tracklist reported a track that is not a video track"
    );
    assert!(
        track.video.is_some(),
        "a video track came back without the video member of the union"
    );
    assert!(
        track.audio.is_none() && track.subtitle.is_none(),
        "a video track came back with a member its type does not name"
    );
    track
}

/// A sample's geometry is the file's, or all six of its fields are absent.
///
/// Those are the two answers this runtime gives -- see the test above -- and a third
/// would be a value libvlc invented, which is the one thing a caller cannot guard
/// against from the outside.
fn assert_sample_geometry(track: &TrackInfo, decoder: &str) {
    let video = track.video.as_ref().expect("checked by video_track_of");
    let geometry = (
        video.width,
        video.height,
        video.sar_num,
        video.sar_den,
        video.frame_rate_num,
        video.frame_rate_den,
    );
    let absent = (0, 0, 0, 0, 0, 0);
    let in_the_file = (
        SAMPLE_WIDTH,
        SAMPLE_HEIGHT,
        SAMPLE_SAR.0,
        SAMPLE_SAR.1,
        SAMPLE_FRAME_RATE.0,
        SAMPLE_FRAME_RATE.1,
    );
    assert!(
        geometry == absent || geometry == in_the_file,
        "{decoder} reported {geometry:?}; the sample is {in_the_file:?}, and the other \
         answer this runtime gives is {absent:?}"
    );
}

/// The type decides which member of the union is read, and nothing else does.
///
/// This is the half of the track API that has to be right for the rest to be
/// safe: libvlc does not write the members a track's type does not name -- a
/// tracklist asked for [constant VLCTrack.TYPE_UNKNOWN] on the media descriptor
/// can carry a union libvlc never wrote at all -- so a reader that tested the
/// pointer instead of the type would hand back heap memory. Every type is asked
/// here, including the ones with no member, and each answer is checked against
/// the type.
///
/// It uses the second sample, which is also where the two things the small
/// sample cannot show come from: a pixel aspect ratio that is not 1:1, and an
/// audio track.
#[test]
fn the_type_decides_which_member_of_the_union_is_read() {
    let sample = Sample::from_path(path_of(SECOND_SAMPLE));
    sample.attach_media();
    play_until_playing(&sample);

    let mut seen = 0;
    for track_type in [
        libvlc_track_type_t_libvlc_track_unknown,
        libvlc_track_type_t_libvlc_track_audio,
        libvlc_track_type_t_libvlc_track_video,
        libvlc_track_type_t_libvlc_track_text,
    ] {
        for track in sample.player_tracks(track_type, false) {
            seen += 1;
            let is_video = track.i_type == libvlc_track_type_t_libvlc_track_video as i32;
            let is_audio = track.i_type == libvlc_track_type_t_libvlc_track_audio as i32;
            let is_text = track.i_type == libvlc_track_type_t_libvlc_track_text as i32;
            assert_eq!(
                track.video.is_some(),
                is_video,
                "a track of type {} disagrees with its own video member",
                track.i_type
            );
            assert_eq!(
                track.audio.is_some(),
                is_audio,
                "a track of type {} disagrees with its own audio member",
                track.i_type
            );
            assert_eq!(
                track.subtitle.is_some(),
                is_text,
                "a track of type {} disagrees with its own subtitle member",
                track.i_type
            );
        }
    }
    assert!(
        seen >= 2,
        "the second sample has a video track and an audio track; {seen} tracks were reported"
    );

    // The player's own tracklist, which is the one a game reads while a media is
    // playing. Its size arrives with the decoder's format report and not with the
    // track: libvlc publishes a *new* track when that happens rather than
    // changing the one it already handed out, which is why this asks again
    // instead of holding the first answer.
    let player_video = wait_for_known_size(
        || sample.player_tracks(libvlc_track_type_t_libvlc_track_video, false),
        Duration::from_secs(5),
    );
    assert_eq!(
        player_video.len(),
        1,
        "the second sample has one video track on the player's tracklist"
    );
    let player_video = player_video[0]
        .video
        .as_ref()
        .expect("the player's video track lost its video member");
    assert_eq!(
        (player_video.width, player_video.height),
        (SECOND_SAMPLE_WIDTH, SECOND_SAMPLE_HEIGHT),
        "the player's tracklist does not report the size the file has"
    );

    let video = wait_for_known_size(
        || sample.media_tracks(libvlc_track_type_t_libvlc_track_video),
        Duration::from_secs(5),
    );
    assert_eq!(
        video.len(),
        1,
        "the media descriptor reported {} video tracks after it had been played",
        video.len()
    );
    let video = video[0]
        .video
        .as_ref()
        .expect("the media descriptor's video track lost its video member");
    assert_eq!(
        (video.width, video.height),
        (SECOND_SAMPLE_WIDTH, SECOND_SAMPLE_HEIGHT),
        "the media path reported a different visible size than the player path"
    );
    assert_eq!(
        (video.sar_num, video.sar_den),
        (SECOND_SAMPLE_SAR_NUM, SECOND_SAMPLE_SAR_DEN),
        "the sample's pixel aspect ratio is 1280:1281; this reports something else"
    );

    let audio = sample.wait_for_track(
        libvlc_track_type_t_libvlc_track_audio,
        Duration::from_secs(5),
    );
    assert_eq!(audio.len(), 1, "the second sample has one audio track");
    let audio = audio[0]
        .audio
        .as_ref()
        .expect("an audio track came back without the audio member of the union");
    assert_eq!(
        (audio.channels, audio.rate),
        (SECOND_SAMPLE_CHANNELS, SECOND_SAMPLE_RATE),
        "the audio track does not describe the AAC stereo stream in the file"
    );
    println!(
        "the second sample: {}x{}, sar {}/{}, {} channels at {} Hz",
        SECOND_SAMPLE_WIDTH,
        SECOND_SAMPLE_HEIGHT,
        SECOND_SAMPLE_SAR_NUM,
        SECOND_SAMPLE_SAR_DEN,
        audio.channels,
        audio.rate
    );

    // The two fields the header reserves for a track that came from a player.
    for track in sample.media_tracks(libvlc_track_type_t_libvlc_track_video) {
        assert_eq!(
            track.name, "",
            "a track from a media descriptor reported a name, which libvlc documents as player-only"
        );
        assert!(
            !track.selected,
            "a track from a media descriptor reported itself selected"
        );
    }
}

/// A text track reports the encoding it declares, which is the only clue a caller
/// has when the subtitle comes out as mojibake.
///
/// libvlc fills `psz_encoding` only when the subtitle's own demuxer advertised
/// one and hands back NULL otherwise, so which of the two happens for a plain
/// UTF-8 SRT is a measurement: the assertion below is that measurement, and the
/// `""` the binding reports for NULL is what a caller sees for the other case.
#[test]
fn a_text_track_reports_the_encoding_it_declares() {
    let sample = Sample::new();
    let subtitle = subtitle_mrl();
    assert_eq!(
        sample.add_slave_to_media(&subtitle, 4),
        0,
        "libvlc refused a subtitle for the media"
    );
    sample.attach_media();
    play_until_playing(&sample);

    let tracks = sample.wait_for_track(
        libvlc_track_type_t_libvlc_track_text,
        Duration::from_secs(5),
    );
    assert_eq!(
        tracks.len(),
        1,
        "the attached subtitle did not become a text track"
    );
    let track = &tracks[0];
    let subtitle = track
        .subtitle
        .as_ref()
        .expect("a text track came back without the subtitle member of the union");
    assert!(
        track.video.is_none() && track.audio.is_none(),
        "a text track came back with a member its type does not name"
    );
    println!(
        "the attached subtitle declares the encoding {:?}",
        subtitle.encoding
    );
    assert_eq!(
        subtitle.encoding, "",
        "a plain UTF-8 SRT declared an encoding; libvlc fills this only when the \
         subtitle demuxer advertises one, and the measured answer for this fixture \
         is that it does not"
    );
}
