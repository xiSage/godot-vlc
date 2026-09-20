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

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::vlc::*;
use crate::vlc_track::{TrackInfo, c_string, read_info};
use printf::printf;

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
        Self::from_path_with(path, &[])
    }

    /// The same, with options added to the instance.
    ///
    /// One test needs an instance option rather than a media player call: the
    /// `audio-desync` option seeds the same value the audio delay API writes, in a
    /// different unit, and that is the only way to see both halves of it.
    fn from_path_with(path: PathBuf, extra: &[*const c_char]) -> Self {
        let path = CString::new(path.to_str().expect("the sample path is not valid UTF-8"))
            .expect("the sample path contains a NUL byte");

        // No audio and no window: neither is what this is about, and a build
        // machine has no sound device.
        let mut options: Vec<*const c_char> = headless_options().to_vec();
        options.extend_from_slice(extra);
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

    fn set_audio_delay(&self, delay_us: i64) -> i32 {
        unsafe { libvlc_audio_set_delay(self.player, delay_us) }
    }

    fn audio_delay(&self) -> i64 {
        unsafe { libvlc_audio_get_delay(self.player) }
    }

    fn jump_time(&self, delta_ms: i64) -> i32 {
        unsafe { libvlc_media_player_jump_time(self.player, delta_ms) }
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

/// Two more subtitles, for the tests that need several text tracks at once. They
/// differ in their cues, so that a test can say which of them it is looking at, and
/// they are three because libvlc's own cap is two: the third is what makes the cap
/// observable rather than a claim about the source.
const SECOND_SUBTITLE: &str = "1\n00:00:00,000 --> 00:00:01,000\nCCCC\n";
const THIRD_SUBTITLE: &str = "1\n00:00:00,000 --> 00:00:01,000\nDDDD\n";
const SECOND_SUBTITLE_FILE: &str = "target/acceptance/subtitle2.srt";
const THIRD_SUBTITLE_FILE: &str = "target/acceptance/subtitle3.srt";

/// Writes one subtitle fixture and returns a `file://` MRL for it.
///
/// A slave takes a URI, so this is the form a subtitle on disk is handed over in --
/// and the one VobSub pairs need, since that demux finds its second file by rewriting
/// the path it was given. Note that a path is not an MRL: a Windows path handed over
/// as one would be split at its drive colon, which is why the `file:///` is built
/// here. Nothing in the test path needs percent-encoding; a path that did (a space, a
/// `#`, a `%`) would, and `VLCSubtitle.load_from_file` is what does that for callers.
fn subtitle_mrl_from(relative: &str, content: &str) -> CString {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the subtitle directory could not be created");
    }
    std::fs::write(&path, content).expect("the subtitle fixture could not be written");

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

fn subtitle_mrl() -> CString {
    subtitle_mrl_from(SUBTITLE_FILE, SAMPLE_SUBTITLE)
}

/// Where the playlist fixture is written, and the `file://` URI it is asked for by.
const PLAYLIST_FILE: &str = "target/acceptance/sample.m3u";

/// Writes a one-entry playlist and returns the `file://` URI that names it.
///
/// A playlist is text, so unlike the media fixtures it costs nothing to keep out of
/// the repository: it is generated next to the subtitles, and its one entry is the
/// sample. The URI is what a media has to be built from for libvlc to see a
/// playlist at all -- a path would be turned into a `file://` URI as well, but the
/// point of this fixture is to name the media by URI so the test can also say what
/// [`libvlc_media_get_mrl`] reports for it.
fn playlist_uri() -> String {
    let entry = sample_path()
        .to_str()
        .expect("the sample path is not valid UTF-8")
        .replace('\\', "/");
    let entry = if entry.starts_with('/') {
        format!("file://{entry}")
    } else {
        format!("file:///{entry}")
    };

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PLAYLIST_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the playlist directory could not be created");
    }
    std::fs::write(&path, format!("#EXTM3U\n{entry}\n"))
        .expect("the playlist fixture could not be written");

    let path = path
        .to_str()
        .expect("the playlist path is not valid UTF-8")
        .replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

/// The MRL libvlc reports for a media, copied out and freed the way a caller has to.
///
/// `libvlc_media_get_mrl` hands back a copy of its own string with no release
/// function of its own, so `libvlc_free` is the only correct disposal -- which is
/// what the binding does too, and what this mirrors.
fn mrl_of(media: *mut libvlc_media_t) -> String {
    let mrl = unsafe { libvlc_media_get_mrl(media) };
    if mrl.is_null() {
        return String::new();
    }
    let copied = unsafe { CStr::from_ptr(mrl) }
        .to_str()
        .expect("the MRL is not valid UTF-8")
        .to_string();
    unsafe { libvlc_free(mrl as *mut c_void) };
    copied
}

/// The type libvlc reports for a media.
fn type_of(media: *mut libvlc_media_t) -> libvlc_media_type_t {
    unsafe { libvlc_media_get_type(media) }
}

/// Plays a media until libvlc reports the type the parse was supposed to give it.
///
/// The type is what is waited for here, not the length: a playlist media reports no
/// length of its own -- its entries have those -- so waiting for one would wait for
/// the timeout every time.
fn play_until_typed(
    instance: *mut libvlc_instance_t,
    media: *mut libvlc_media_t,
    wanted: libvlc_media_type_t,
) -> libvlc_media_type_t {
    unsafe {
        let player = libvlc_media_player_new(instance);
        assert!(!player.is_null(), "libvlc_media_player_new returned NULL");
        libvlc_media_player_set_media(player, media);
        assert_eq!(
            libvlc_media_player_play(player),
            0,
            "the playback was refused"
        );

        let deadline = Instant::now() + PLAYBACK_TIMEOUT;
        let mut current = type_of(media);
        while Instant::now() < deadline && current != wanted {
            std::thread::sleep(Duration::from_millis(10));
            current = type_of(media);
        }

        libvlc_media_player_stop_async(player);
        libvlc_media_player_release(player);
        current
    }
}

/// The length libvlc reports for a media, after playing it once.
///
/// The media is borrowed, not owned: its own player is built here and released
/// again, so a test can measure two medias -- or the same one twice -- without
/// sharing a player between them.
fn reported_length(instance: *mut libvlc_instance_t, media: *mut libvlc_media_t) -> i64 {
    unsafe {
        let player = libvlc_media_player_new(instance);
        assert!(!player.is_null(), "libvlc_media_player_new returned NULL");
        libvlc_media_player_set_media(player, media);
        assert_eq!(
            libvlc_media_player_play(player),
            0,
            "the playback was refused"
        );

        // The length arrives with the parse, which is what this is waiting for: a
        // media whose length never appears would make the assertions below
        // meaningless rather than failing.
        let deadline = Instant::now() + PLAYBACK_TIMEOUT;
        let mut length = 0;
        while Instant::now() < deadline {
            length = libvlc_media_player_get_length(player);
            if length > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        libvlc_media_player_stop_async(player);
        libvlc_media_player_release(player);
        length
    }
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

    /// Waits until the player reports at least `wanted` tracks of this type.
    fn wait_for_tracks(
        &self,
        track_type: libvlc_track_type_t,
        wanted: usize,
        timeout: Duration,
    ) -> Vec<TrackInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let tracks = self.player_tracks(track_type, false);
            if tracks.len() >= wanted || Instant::now() >= deadline {
                return tracks;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The tracks of one type that are selected right now.
    fn selected_tracks(&self, track_type: libvlc_track_type_t) -> Vec<TrackInfo> {
        self.player_tracks(track_type, true)
    }

    /// Reads a track libvlc hands over as the caller's own, and releases it.
    ///
    /// `get_selected_track` and `get_track_from_id` return a reference that belongs
    /// to the caller, unlike a tracklist entry, which is released with its list.
    fn read_owned_track(&self, ptr: *mut libvlc_media_track_t) -> Option<TrackInfo> {
        if ptr.is_null() {
            return None;
        }
        let info = unsafe { read_info(ptr) };
        unsafe { libvlc_media_track_release(ptr) };
        Some(info)
    }

    /// The track libvlc reports as this type's selected one.
    fn selected_track(&self, track_type: libvlc_track_type_t) -> Option<TrackInfo> {
        let ptr = unsafe { libvlc_media_player_get_selected_track(self.player, track_type) };
        self.read_owned_track(ptr)
    }

    /// The track with this id, if the current input has one.
    fn track_from_id(&self, id: &CStr) -> Option<TrackInfo> {
        let ptr = unsafe { libvlc_media_player_get_track_from_id(self.player, id.as_ptr()) };
        self.read_owned_track(ptr)
    }

    /// Selects a whole set of tracks of one type, by their position in the current
    /// tracklist -- the way the binding's `select_tracks` replaces a selection.
    ///
    /// The pointers come from a tracklist, and libvlc keeps the tracks' `es_id`s
    /// rather than the tracks themselves, so that list is released as soon as the
    /// call returns.
    fn select_tracks(&self, track_type: libvlc_track_type_t, wanted: &[usize]) {
        let list = unsafe { libvlc_media_player_get_tracklist(self.player, track_type, false) };
        assert!(
            !list.is_null(),
            "the player reported no tracklist for type {track_type} to select from"
        );
        let mut pointers: Vec<*const libvlc_media_track_t> = Vec::with_capacity(wanted.len());
        for index in wanted {
            let track = unsafe { libvlc_media_tracklist_at(list, *index) };
            assert!(
                !track.is_null(),
                "the tracklist has no track at index {index}"
            );
            pointers.push(track);
        }
        unsafe {
            libvlc_media_player_select_tracks(
                self.player,
                track_type,
                pointers.as_mut_ptr(),
                pointers.len(),
            );
            libvlc_media_tracklist_delete(list);
        }
    }

    /// Selects by id, the way the binding's `select_tracks_by_ids` does: one
    /// comma-separated string, which libvlc stores on the player.
    fn select_tracks_by_ids(&self, track_type: libvlc_track_type_t, ids: &CStr) {
        unsafe {
            libvlc_media_player_select_tracks_by_ids(self.player, track_type, ids.as_ptr());
        }
    }

    /// Waits until libvlc reports exactly `wanted` selected tracks of a type.
    ///
    /// A selection is queued to libvlc's input thread, so it does not land with the
    /// call that asked for it: a test has to wait for the state, exactly as a caller
    /// has to wait for the signal.
    fn wait_for_selection(
        &self,
        track_type: libvlc_track_type_t,
        wanted: usize,
        timeout: Duration,
    ) -> Vec<TrackInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let selected = self.selected_tracks(track_type);
            if selected.len() == wanted || Instant::now() >= deadline {
                return selected;
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

/// The audio delay belongs to the input, exactly as the subtitle one does.
///
/// It is also the one `libvlc_audio_*` setting that needs no audio output: the
/// value is kept on the input and handed to the decoders, so this harness -- whose
/// instance is built with `--no-audio` -- can still set it and read it back. What it
/// cannot show is an audible effect, and nothing here claims one.
#[test]
fn the_audio_delay_dies_with_the_input_and_needs_no_audio_output() {
    let sample = Sample::new();

    assert_eq!(
        sample.set_audio_delay(250_000),
        0,
        "the delay was refused for a player with no input"
    );
    assert_eq!(
        sample.audio_delay(),
        0,
        "a delay set before there was an input was kept somewhere"
    );

    sample.attach_media();
    play_until_playing(&sample);

    assert_eq!(
        sample.set_audio_delay(-250_000),
        0,
        "the delay was refused while playing"
    );
    assert_eq!(
        sample.audio_delay(),
        -250_000,
        "the delay did not read back while playing, so a player with no audio output \
         treats it differently from one with"
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
        sample.audio_delay(),
        0,
        "the delay outlived the input it was set on"
    );
}

/// The instance option `audio-desync` seeds that same value, in milliseconds.
///
/// This is the unit trap in one assertion: the option is documented as milliseconds,
/// this API is microseconds, and both write the same field of the input, so
/// `--audio-desync=250` has to come back as `250000`.
#[test]
fn the_audio_desync_option_seeds_the_delay_in_milliseconds() {
    let sample = Sample::from_path_with(sample_path(), &[c"--audio-desync=250".as_ptr()]);

    sample.attach_media();
    play_until_playing(&sample);

    assert_eq!(
        sample.audio_delay(),
        250_000,
        "the option did not seed the input, or it and this API disagree about the unit"
    );

    // And a value set through the API replaces it rather than adding to it.
    assert_eq!(sample.set_audio_delay(100_000), 0, "the delay was refused");
    assert_eq!(
        sample.audio_delay(),
        100_000,
        "setting a delay added to what the option seeded instead of replacing it"
    );
}

/// `jump_time` moves by a delta from wherever playback is, and reports nothing.
///
/// Three of its four edges are pinned here: a jump that lands past the start is
/// clamped to the input's own first tick and is not an error, a jump with no input
/// does nothing at all, and the return value is `0` on every path even though the
/// header promises `-1 on error`. The fourth, a jump past the end, is the demuxer's
/// decision and nothing reports what it decided -- what is pinned is the measurement.
#[test]
fn jumping_moves_by_a_delta_and_stops_at_the_start() {
    // No input at all: accepted, dropped, and reported as success.
    let idle = Sample::new();
    assert_eq!(
        idle.jump_time(10_000),
        0,
        "a jump with no input was refused; the header says only -1 is an error"
    );
    assert_eq!(
        idle.time(),
        0,
        "a jump with no input moved something after all"
    );

    let sample = Sample::from_path(path_of(SECOND_SAMPLE));
    sample.attach_media();
    play_until_playing(&sample);

    // Forward, from wherever playback happens to be. The second sample is minutes
    // long, so three seconds forward is not near an edge.
    let before = sample.time();
    assert_eq!(sample.jump_time(3_000), 0, "the forward jump was refused");
    assert!(
        wait_for_time(&sample, before + 3_000, PLAYBACK_TIMEOUT),
        "the clock never reached {} ms after jumping forward from {before} ms; it is at {} ms",
        before + 3_000,
        sample.time()
    );

    // Backwards, past the start: clamped, not refused.
    assert_eq!(
        sample.jump_time(-600_000),
        0,
        "the backward jump was refused"
    );
    assert!(
        wait_for_time_at_most(&sample, 1_000, PLAYBACK_TIMEOUT),
        "a jump past the start did not land at the start; the clock is at {} ms",
        sample.time()
    );
    println!(
        "jumping: 600 s back from {} landed at {} ms",
        before + 3_000,
        sample.time()
    );

    // Past the end: libvlc does not clamp this. It hands the value to the demuxer,
    // which goes to the end of the stream, and playback then runs out of input --
    // measured on this runtime, the same answer four runs out of four: the player
    // ends up Stopped with its clock back at 0. The return value says none of that.
    let length = unsafe { libvlc_media_player_get_length(sample.player) };
    assert_eq!(
        sample.jump_time(length + 60_000),
        0,
        "a jump past the end was refused, although libvlc returns 0 on every path"
    );
    assert!(
        sample.wait_for_state(libvlc_state_t_libvlc_Stopped as i32, PLAYBACK_TIMEOUT),
        "a jump past the end did not end playback; it is in state {} with its clock at {} ms",
        sample.state(),
        sample.time()
    );
    assert_eq!(
        sample.time(),
        0,
        "the jump past the end ended playback but left the clock somewhere else"
    );
}

/// Waits until the player's clock reads at least `ms`.
fn wait_for_time(sample: &Sample, ms: i64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if sample.time() >= ms {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Waits until the player's clock reads at most `ms`.
fn wait_for_time_at_most(sample: &Sample, ms: i64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if sample.time() <= ms {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Collects what a time watcher reported.
///
/// Nothing else may happen in the callbacks that fill this: they run on libvlc's
/// threads -- the output thread that displayed the frame or wrote the samples, or the
/// input thread -- and libvlc holds the player's timer lock while it calls them.
#[derive(Default)]
struct WatchProbe {
    points: Mutex<Vec<libvlc_media_player_time_point_t>>,
    paused: Mutex<Vec<i64>>,
    seeks: Mutex<Vec<Option<libvlc_media_player_time_point_t>>>,
}

impl WatchProbe {
    fn new() -> Box<Self> {
        Box::new(Self::default())
    }

    /// The address to hand libvlc as the callbacks' data.
    fn data(&mut self) -> *mut c_void {
        self as *mut Self as *mut c_void
    }

    fn points(&self) -> Vec<libvlc_media_player_time_point_t> {
        self.points
            .lock()
            .expect("the probe lock was poisoned")
            .clone()
    }

    fn point_count(&self) -> usize {
        self.points
            .lock()
            .expect("the probe lock was poisoned")
            .len()
    }

    fn seeks(&self) -> Vec<Option<libvlc_media_player_time_point_t>> {
        self.seeks
            .lock()
            .expect("the probe lock was poisoned")
            .clone()
    }
}

unsafe extern "C" fn watch_point(
    value: *const libvlc_media_player_time_point_t,
    data: *mut c_void,
) {
    unsafe {
        let probe = &*(data as *const WatchProbe);
        if let Some(point) = value.as_ref() {
            probe
                .points
                .lock()
                .expect("the probe lock was poisoned")
                .push(*point);
        }
    }
}

unsafe extern "C" fn watch_paused(system_date_us: i64, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const WatchProbe);
        probe
            .paused
            .lock()
            .expect("the probe lock was poisoned")
            .push(system_date_us);
    }
}

unsafe extern "C" fn watch_seek(value: *const libvlc_media_player_time_point_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const WatchProbe);
        probe
            .seeks
            .lock()
            .expect("the probe lock was poisoned")
            .push(value.as_ref().copied());
    }
}

/// Registers the watcher on a player, with the probe as its data.
fn watch(sample: &Sample, probe: &mut WatchProbe, min_period_us: i64) -> i32 {
    unsafe {
        libvlc_media_player_watch_time(
            sample.player,
            min_period_us,
            Some(watch_point),
            Some(watch_paused),
            Some(watch_seek),
            probe.data(),
        )
    }
}

/// Waits until the probe has collected `wanted` points, or `timeout` passes.
fn wait_for_points(probe: &WatchProbe, wanted: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe.point_count() >= wanted {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The time watcher reports a point for every displayed frame or written block, one
/// watcher per player, and it stops when it is taken off.
///
/// This is also the measurement behind the binding's `time_point` signal: the harness
/// builds every instance with `--no-audio --vout=dummy`, and the watcher fires anyway
/// -- about twenty times a second for this ten-frame-a-second media, because the input
/// clock reports as well as the output. Nothing here claims a rate beyond "it fires,
/// and often enough that a frame's worth of it is worth coalescing".
#[test]
fn the_time_watcher_reports_points_and_only_one_can_be_registered() {
    let sample = Sample::from_path(path_of(SECOND_SAMPLE));
    sample.attach_media();
    let mut probe = WatchProbe::new();

    assert_eq!(
        watch(&sample, &mut probe, 0),
        0,
        "the watcher was refused on a player with no watcher and nothing playing"
    );
    assert_eq!(
        watch(&sample, &mut probe, 0),
        -1,
        "a second watcher was accepted; libvlc's header says the second call fails"
    );

    play_until_playing(&sample);
    assert!(
        wait_for_points(&probe, 8, PLAYBACK_TIMEOUT),
        "the watcher reported {} points in {PLAYBACK_TIMEOUT:?}, which is not a running clock",
        probe.point_count()
    );
    let points = probe.points();
    println!(
        "the watcher reported {} points; the first is {:?} and the last {:?}",
        points.len(),
        points.first(),
        points.last()
    );

    // Every point is a point: a media time, a position within the media, a rate, a
    // length from the container, and a system date -- which is either a real date on
    // libvlc's clock or the "the clock was paused" sentinel.
    let now = unsafe { libvlc_clock() };
    for point in &points {
        assert!(
            point.ts_us >= -1,
            "a point reported a media time of {} us, outside libvlc's `>= 0 or -1`",
            point.ts_us
        );
        assert!(
            (0.0..=1.0).contains(&point.position),
            "a point reported position {}, outside 0.0-1.0",
            point.position
        );
        assert!(point.rate > 0.0, "a point reported rate {}", point.rate);
        assert!(
            point.system_date_us == i64::MAX || point.system_date_us <= now,
            "a point reports a system date {} ahead of the clock's {now} without being the paused sentinel",
            point.system_date_us
        );
    }
    assert!(
        points.iter().any(|point| point.system_date_us != i64::MAX),
        "every point claimed the clock was paused, which cannot be true of a running playback"
    );

    let before = probe.point_count();
    unsafe { libvlc_media_player_unwatch_time(sample.player) };
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        probe.point_count(),
        before,
        "the watcher reported more points after being taken off"
    );
}

/// A larger report period means fewer reports: `min_period_us` really is a floor.
///
/// `0` asks libvlc for everything its sources produce; a large value asks it to hold
/// most of them back. Both are counted on one playback, so the media, the output and
/// the machine are the same for the two.
#[test]
fn a_larger_min_period_reports_less_often() {
    let sample = Sample::from_path(path_of(SECOND_SAMPLE));
    sample.attach_media();
    play_until_playing(&sample);

    let mut every = WatchProbe::new();
    assert_eq!(watch(&sample, &mut every, 0), 0, "the watcher was refused");
    std::thread::sleep(Duration::from_millis(1200));
    let all = every.point_count();
    unsafe { libvlc_media_player_unwatch_time(sample.player) };

    let mut sparse = WatchProbe::new();
    assert_eq!(
        watch(&sample, &mut sparse, 500_000),
        0,
        "the watcher was refused the second time, after being taken off once"
    );
    std::thread::sleep(Duration::from_millis(1200));
    let few = sparse.point_count();
    unsafe { libvlc_media_player_unwatch_time(sample.player) };

    println!("with min_period 0: {all} points in 1.2 s; with 500 ms: {few}");
    assert!(
        all >= 4,
        "a period of 0 produced {all} points in 1.2 s, which is not a running clock"
    );
    assert!(
        few < all,
        "asking for a 500 ms period produced {few} points against {all} for no period at all"
    );
}

/// One seek is reported twice: the point it was asked for, then the end of it.
///
/// libvlc calls its seek callback twice for one seek, and the second call carries no
/// point at all -- which is what the binding splits into `time_point_seek` and
/// `time_point_seek_finished`.
#[test]
fn a_seek_is_reported_twice_by_the_watcher() {
    let sample = Sample::from_path(path_of(SECOND_SAMPLE));
    sample.attach_media();
    play_until_playing(&sample);
    let mut probe = WatchProbe::new();
    assert_eq!(watch(&sample, &mut probe, 0), 0, "the watcher was refused");
    assert!(
        wait_for_points(&probe, 1, PLAYBACK_TIMEOUT),
        "no point arrived before the seek"
    );

    assert_eq!(
        unsafe { libvlc_media_player_set_time(sample.player, 30_000, false) },
        0,
        "the seek was refused"
    );
    let deadline = Instant::now() + PLAYBACK_TIMEOUT;
    while Instant::now() < deadline && probe.seeks().len() < 2 {
        std::thread::sleep(Duration::from_millis(10));
    }
    let seeks = probe.seeks();
    println!(
        "the watcher reported {} seek events: {seeks:?}",
        seeks.len()
    );
    assert_eq!(
        seeks.len(),
        2,
        "one seek produced {} seek events rather than the request and the end of it",
        seeks.len()
    );
    assert!(
        seeks[0].is_some(),
        "the first seek event carried no point, so the seek's own target was never reported"
    );
    assert!(
        seeks[1].is_none(),
        "the second seek event carried a point; libvlc sends NULL when the seek is over"
    );
    let target = seeks[0].expect("checked above");
    assert!(
        (target.ts_us - 30_000_000).abs() < 1_000_000,
        "the seek was asked for 30 s and reported as {} us",
        target.ts_us
    );
    unsafe { libvlc_media_player_unwatch_time(sample.player) };
}

/// Interpolation advances a point by the time since it was made, and refuses when that
/// lands before the media's start.
///
/// The points here are built by hand rather than captured, because what is under test
/// is arithmetic: a captured point would make the expected value depend on how long the
/// test took. The three cases are the ones the binding's `interpolate_time_point` rests
/// on -- a point from the past, a paused-clock point (`INT64_MAX`, which libvlc answers
/// with the point unchanged), and a point dated **ahead** of the clock it is read
/// against, which is how the media time comes out negative. libvlc's own header says a
/// point's date "can be in the future or in the past", and this is what that costs:
/// `VLC_EGENERIC` -- which is what libvlc answers where its header promises `-1` -- and
/// **an out-parameter holding its own uninitialised stack**. That last part is why the
/// binding passes its own sentinels in and reports them, rather than reading whatever
/// the call left behind.
#[test]
fn interpolating_a_point_uses_the_system_clock_and_refuses_a_negative_result() {
    let point = libvlc_media_player_time_point_t {
        position: 0.5,
        rate: 1.0,
        ts_us: 5_000_000,
        length_us: 10_000_000,
        system_date_us: 1_000_000,
    };

    // One second of system time later, at rate 1.0: the media time has moved a second.
    let mut ts_us: i64 = -2;
    let mut position: f64 = -2.0;
    let status = unsafe {
        libvlc_media_player_time_point_interpolate(&point, 2_000_000, &mut ts_us, &mut position)
    };
    assert_eq!(status, 0, "the interpolation of a fresh point was refused");
    assert_eq!(
        ts_us, 6_000_000,
        "one second of clock did not move the media time by one second"
    );
    assert!(
        (position - 0.6).abs() < 0.0001,
        "the interpolated position is {position}, not the point's 0.5 plus a tenth"
    );

    // The same point at twice the rate moves twice as far.
    let fast = libvlc_media_player_time_point_t { rate: 2.0, ..point };
    let status = unsafe {
        libvlc_media_player_time_point_interpolate(&fast, 2_000_000, &mut ts_us, &mut position)
    };
    assert_eq!(status, 0, "the interpolation at a doubled rate was refused");
    assert_eq!(ts_us, 7_000_000, "twice the rate did not move twice as far");

    // A paused clock has nothing to interpolate: the point comes back unchanged.
    let paused = libvlc_media_player_time_point_t {
        system_date_us: i64::MAX,
        ..point
    };
    let status = unsafe {
        libvlc_media_player_time_point_interpolate(&paused, 2_000_000, &mut ts_us, &mut position)
    };
    assert_eq!(status, 0, "a paused-clock point was refused");
    assert_eq!(
        (ts_us, position),
        (5_000_000, 0.5),
        "a paused-clock point was interpolated instead of returned as it was"
    );

    // A point dated ahead of the clock it is read against: its media time would come
    // out negative, so libvlc refuses. What its two out-parameters hold then is the
    // reason the binding discards both on a non-zero status: its core returns before
    // writing either, but the libvlc wrapper writes `out_ts_us` anyway, from a local the
    // core never filled. The caller therefore gets *stack memory* back -- measured:
    // 292914458344 in one run and another number in the next -- while `position` is left
    // as it was. Nothing here can assert what that number is; what is asserted is that it
    // is not the caller's value, because that is what makes it untrustworthy.
    let ahead = libvlc_media_player_time_point_t {
        ts_us: 100,
        system_date_us: 10_000_000,
        ..point
    };
    ts_us = -7;
    position = -7.0;
    let status =
        unsafe { libvlc_media_player_time_point_interpolate(&ahead, 1, &mut ts_us, &mut position) };
    assert_eq!(
        status,
        i32::MIN,
        "interpolating a point dated 10 s ahead, which lands at a negative media time, was accepted"
    );
    assert_ne!(
        ts_us, -7,
        "libvlc's wrapper is supposed to write out_ts_us on every path, including the one \
         where its core wrote nothing, and this one did not"
    );
    assert_eq!(
        position, -7.0,
        "libvlc wrote the position on the path where it reports failure, although its core \
         returns before it touches that parameter"
    );
    println!("a refused interpolation answers {status} and leaves ts_us at {ts_us}");
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

/// One track event, as a test read it out of libvlc's event object.
#[derive(Clone, Debug)]
struct RecordedTrackEvent {
    event_type: libvlc_event_type_t,
    track_type: i32,
    /// The track the event names: `psz_id` for the three list events, and the track
    /// that joined the selection for `ESSelected`.
    id: String,
    /// The track `ESSelected` reports as having left the selection; `""` for the
    /// three list events, and for a selection event that is about a track joining.
    unselected_id: String,
}

/// Where the track events land. Written from libvlc's input thread.
#[derive(Default)]
struct TrackEventProbe {
    events: Mutex<Vec<RecordedTrackEvent>>,
}

impl TrackEventProbe {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<RecordedTrackEvent> {
        match self.events.lock() {
            Ok(events) => events.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Waits for the first recorded event that matches, and returns it.
    ///
    /// It panics with everything it saw when nothing matches, because the useful
    /// part of a failure here is which events did arrive.
    fn wait_for<F>(&self, what: &str, timeout: Duration, matches: F) -> RecordedTrackEvent
    where
        F: Fn(&RecordedTrackEvent) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let events = self.snapshot();
            if let Some(found) = events.iter().find(|event| matches(event)) {
                return found.clone();
            }
            if Instant::now() >= deadline {
                panic!("no {what} arrived within {timeout:?}; the events seen were {events:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Records one track event. Written from libvlc's input thread, holding the
/// player's lock, so nothing here may touch the player or a Godot object.
unsafe extern "C" fn record_track_event(event: *const libvlc_event_t, user_data: *mut c_void) {
    unsafe {
        let probe = &*(user_data as *const TrackEventProbe);
        let event_type = (*event).type_;
        let recorded =
            if event_type == libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t {
                let payload = (*event).u.media_player_es_selection_changed;
                RecordedTrackEvent {
                    event_type,
                    track_type: payload.i_type as i32,
                    id: c_string(payload.psz_selected_id),
                    unselected_id: c_string(payload.psz_unselected_id),
                }
            } else {
                let payload = (*event).u.media_player_es_changed;
                RecordedTrackEvent {
                    event_type,
                    track_type: payload.i_type as i32,
                    id: c_string(payload.psz_id),
                    unselected_id: String::new(),
                }
            };
        let mut events = match probe.events.lock() {
            Ok(events) => events,
            Err(poisoned) => poisoned.into_inner(),
        };
        events.push(recorded);
    }
}

/// The four track events arrive, and say which track and which direction.
///
/// This is the half of the track signals that can be pinned without an engine: the
/// binding's callbacks read exactly these payloads, so a runtime that stopped
/// raising the events -- or that filled the selection event's two ids the other way
/// round -- would leave `track_added` and its four siblings silent or wrong.
#[test]
fn the_track_events_reach_a_callback_with_their_payloads() {
    // The probe is declared first so that it outlives the player: locals drop in
    // reverse order, and releasing the player is what stops the callbacks.
    let mut probe = Box::new(TrackEventProbe::new());
    let opaque = probe.as_mut() as *mut TrackEventProbe as *mut c_void;
    let sample = Sample::new();

    let event_manager = unsafe { libvlc_media_player_event_manager(sample.player) };
    for event_type in [
        libvlc_event_e_libvlc_MediaPlayerESAdded,
        libvlc_event_e_libvlc_MediaPlayerESDeleted,
        libvlc_event_e_libvlc_MediaPlayerESUpdated,
        libvlc_event_e_libvlc_MediaPlayerESSelected,
    ] {
        let attached = unsafe {
            libvlc_event_attach(
                event_manager,
                event_type as libvlc_event_type_t,
                Some(record_track_event),
                opaque,
            )
        };
        assert_eq!(
            attached, 0,
            "the track event {event_type} could not be attached"
        );
    }

    sample.attach_media();
    play_until_playing(&sample);

    // The sample's video track is created with the input, and libvlc selects it on
    // its own -- which is what makes the selection event below arrive without
    // anything asking for it.
    let added = probe.wait_for(
        "track_added for the video track",
        Duration::from_secs(5),
        |event| {
            event.event_type == libvlc_event_e_libvlc_MediaPlayerESAdded as libvlc_event_type_t
                && event.track_type == libvlc_track_type_t_libvlc_track_video as i32
        },
    );
    assert!(
        !added.id.is_empty(),
        "the added track came with no id, which is the one thing a handler needs: {added:?}"
    );
    let selected = probe.wait_for(
        "track_selected for the video track",
        Duration::from_secs(5),
        |event| {
            event.event_type == libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t
                && event.id == added.id
        },
    );
    assert!(
        selected.unselected_id.is_empty(),
        "the first selection reported an unselected track as well: {selected:?}"
    );
    println!("the track events so far: {:?}", probe.snapshot());

    // A subtitle attached while playing is another track, and selecting it by id is
    // a selection event naming that id.
    let subtitle = subtitle_mrl();
    assert_eq!(
        sample.add_slave_to_player(&subtitle, false),
        0,
        "the player refused a subtitle while playing"
    );
    let text = probe.wait_for(
        "track_added for the subtitle",
        Duration::from_secs(5),
        |event| {
            event.event_type == libvlc_event_e_libvlc_MediaPlayerESAdded as libvlc_event_type_t
                && event.track_type == libvlc_track_type_t_libvlc_track_text as i32
        },
    );
    let id = CString::new(text.id.clone()).expect("the id contains a NUL byte");
    sample.select_tracks_by_ids(libvlc_track_type_t_libvlc_track_text, &id);
    probe.wait_for(
        "track_selected for the subtitle",
        Duration::from_secs(5),
        |event| {
            event.event_type == libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t
                && event.id == text.id
        },
    );

    // Unselecting it is the other direction of the same event, and the only thing
    // that says which direction it is: the type is the same either way.
    unsafe {
        libvlc_media_player_unselect_track_type(
            sample.player,
            libvlc_track_type_t_libvlc_track_text,
        )
    };
    let unselected = probe.wait_for(
        "track_unselected for the subtitle",
        Duration::from_secs(5),
        |event| {
            event.event_type == libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t
                && event.unselected_id == text.id
        },
    );
    assert!(
        unselected.id.is_empty(),
        "an unselect reported a selected track as well: {unselected:?}"
    );

    // Nothing was deleted or updated in this test, and libvlc raises those events
    // only when they happen.
    let deleted = probe.snapshot().iter().any(|event| {
        event.event_type == libvlc_event_e_libvlc_MediaPlayerESDeleted as libvlc_event_type_t
    });
    assert!(!deleted, "a track was reported as deleted, and none was");
}

/// A type's selection is the set that was asked for, capped where libvlc caps it.
///
/// Text tracks are the interesting case because libvlc allows two of them: it is the
/// only type where a selection of more than one is both allowed and reachable
/// through the engine's own UI. Three subtitles are attached so that the cap can be
/// observed -- asking for all three is answered with two, silently -- and the tests
/// then walk the selection down to one and to none, which is what
/// `select_tracks` and an empty array respectively mean.
#[test]
fn selecting_text_tracks_replaces_the_selection_and_the_cap_is_two() {
    let sample = Sample::new();
    let first = subtitle_mrl();
    let second = subtitle_mrl_from(SECOND_SUBTITLE_FILE, SECOND_SUBTITLE);
    let third = subtitle_mrl_from(THIRD_SUBTITLE_FILE, THIRD_SUBTITLE);
    for (what, subtitle) in [("first", &first), ("second", &second), ("third", &third)] {
        assert_eq!(
            sample.add_slave_to_media(subtitle, 4),
            0,
            "libvlc refused the {what} subtitle"
        );
    }
    sample.attach_media();
    play_until_playing(&sample);

    let tracks = sample.wait_for_tracks(
        libvlc_track_type_t_libvlc_track_text,
        3,
        Duration::from_secs(5),
    );
    assert_eq!(
        tracks.len(),
        3,
        "three subtitles were attached and {} became tracks",
        tracks.len()
    );
    let ids: Vec<String> = tracks.iter().map(|track| track.id.clone()).collect();
    println!("the three attached subtitles are {ids:?}");

    // Asking for all three: libvlc's cap is two, and the extra one is dropped
    // without a word -- the call returns nothing and no event names it.
    sample.select_tracks(libvlc_track_type_t_libvlc_track_text, &[0, 1, 2]);
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        2,
        Duration::from_secs(5),
    );
    assert_eq!(
        selected.len(),
        2,
        "three text tracks were asked for; libvlc's cap is two and it selected {}",
        selected.len()
    );
    let selected_ids: Vec<&String> = selected.iter().map(|track| &track.id).collect();
    for id in &selected_ids {
        assert!(
            ids.contains(id),
            "the selection reported {id}, which is not one of the attached subtitles"
        );
    }

    // One of them alone: the set is replaced, not added to.
    sample.select_tracks(libvlc_track_type_t_libvlc_track_text, &[2]);
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        1,
        Duration::from_secs(5),
    );
    assert_eq!(selected.len(), 1, "selecting one track left a set behind");
    assert_eq!(
        selected[0].id, ids[2],
        "the selection is not the track that was asked for"
    );

    // `get_selected_track` answers the same thing for a single selection.
    let one = sample
        .selected_track(libvlc_track_type_t_libvlc_track_text)
        .expect("a track is selected and get_selected_track answered with nothing");
    assert_eq!(one.id, ids[2], "get_selected_track named another track");

    // An empty array clears the type's selection; so does unselect_track_type,
    // which is the same thing by another name.
    sample.select_tracks(libvlc_track_type_t_libvlc_track_text, &[]);
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        0,
        Duration::from_secs(5),
    );
    assert!(
        selected.is_empty(),
        "an empty selection left {} track(s) selected",
        selected.len()
    );
    assert!(
        sample
            .selected_track(libvlc_track_type_t_libvlc_track_text)
            .is_none(),
        "get_selected_track still found a selected text track"
    );
}

/// An id selects its track, is remembered across a restart, and matches nothing
/// once it does not.
///
/// This is what `VLCTrack.get_id`'s promise rests on, and the reason the header can
/// call an id stable: libvlc keeps the ids a caller selects by on the player, for
/// the media it is playing, and applies them again when the input is created. An id
/// that matches nothing is not an error to libvlc -- it clears the type's selection
/// -- so a saved preference that rots quietly unselects rather than failing.
#[test]
fn an_id_selects_a_track_and_the_choice_survives_a_restart() {
    let sample = Sample::new();
    let subtitle = subtitle_mrl();
    assert_eq!(
        sample.add_slave_to_media(&subtitle, 4),
        0,
        "libvlc refused the subtitle"
    );
    sample.attach_media();
    play_until_playing(&sample);

    let tracks = sample.wait_for_tracks(
        libvlc_track_type_t_libvlc_track_text,
        1,
        Duration::from_secs(5),
    );
    assert_eq!(tracks.len(), 1, "the subtitle did not become a text track");
    let id = CString::new(tracks[0].id.clone()).expect("the id contains a NUL byte");

    sample.select_tracks_by_ids(libvlc_track_type_t_libvlc_track_text, &id);
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        1,
        Duration::from_secs(5),
    );
    assert_eq!(selected.len(), 1, "the id selected nothing");
    assert_eq!(
        selected[0].id, tracks[0].id,
        "the id selected another track"
    );

    // The id is what finds the track again, which is the other half of the same
    // promise.
    let found = sample
        .track_from_id(&id)
        .expect("get_track_from_id did not find the track the id names");
    assert_eq!(
        found.id, tracks[0].id,
        "get_track_from_id answered another id"
    );

    // An id that matches nothing clears the selection: libvlc's rule, and the
    // reason a stale preference fails quietly.
    sample.select_tracks_by_ids(libvlc_track_type_t_libvlc_track_text, c"no/such/track");
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        0,
        Duration::from_secs(5),
    );
    assert!(
        selected.is_empty(),
        "an id that matches nothing left {} selected, and libvlc's rule is that it clears the type",
        selected.len()
    );

    // Select by id again, then stop and start the same media: the choice is stored
    // on the player, so the new input comes up with it already applied.
    sample.select_tracks_by_ids(libvlc_track_type_t_libvlc_track_text, &id);
    sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        1,
        Duration::from_secs(5),
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
    let selected = sample.wait_for_selection(
        libvlc_track_type_t_libvlc_track_text,
        1,
        Duration::from_secs(5),
    );
    assert_eq!(
        selected.len(),
        1,
        "the id stored on the player was not applied to the new input"
    );
    assert_eq!(
        selected[0].id, tracks[0].id,
        "the new input selected another track than the stored id names"
    );
}

/// Without an input, the selection calls do nothing and the getters answer nothing.
///
/// The whole track API above the media descriptor is about the input, and there is
/// none until a media is assigned -- not when `play` is called. libvlc's selection
/// calls return `void`, so this silence is the only thing a caller can observe; the
/// assertions here are that it is silence rather than a crash or a stale answer.
#[test]
fn selecting_without_an_input_changes_nothing() {
    let sample = Sample::new();

    // `select_tracks` with no input: libvlc's own documentation allows a null array
    // when the count is zero, and this is the empty selection it means.
    unsafe {
        libvlc_media_player_select_tracks(
            sample.player,
            libvlc_track_type_t_libvlc_track_text,
            ptr::null_mut(),
            0,
        )
    };
    sample.select_tracks_by_ids(libvlc_track_type_t_libvlc_track_text, c"spu/0");
    unsafe {
        libvlc_media_player_unselect_track_type(
            sample.player,
            libvlc_track_type_t_libvlc_track_text,
        )
    };

    assert!(
        sample
            .selected_track(libvlc_track_type_t_libvlc_track_video)
            .is_none(),
        "a player with no input reported a selected video track"
    );
    assert!(
        sample.track_from_id(c"video/0").is_none(),
        "a player with no input found a track by id"
    );
    assert!(
        sample
            .selected_tracks(libvlc_track_type_t_libvlc_track_text)
            .is_empty(),
        "a player with no input reported selected text tracks"
    );

    // And the calls above were not remembered as a selection: the input this player
    // creates has its own.
    sample.attach_media();
    play_until_playing(&sample);
    assert!(
        !sample
            .player_tracks(libvlc_track_type_t_libvlc_track_text, false)
            .is_empty()
            || sample
                .player_tracks(libvlc_track_type_t_libvlc_track_video, false)
                .len()
                == 1,
        "the input did not come up with the sample's tracks"
    );
}

/// A media reports the MRL it was built from, and the type libvlc guessed from it.
///
/// None of the answers is the string the caller passed in: a path becomes a
/// `file://` URI on the way in, a location is kept verbatim, and a media built on
/// callbacks -- which is what this binding's `load_from_file` does -- always gets
/// the same constant, `imem://`. That last one cannot be reached from here, since
/// it takes the binding's own callbacks; `demo/tests/media_loader.gd` measures it.
#[test]
fn a_media_reports_the_mrl_it_was_built_from_and_the_type_libvlc_guesses() {
    let sample = Sample::new();
    let mrl = mrl_of(sample.media);
    assert!(
        mrl.starts_with("file://") && mrl.ends_with("h264_64x64_1s.mp4"),
        "a media built from a path reports {mrl}"
    );
    assert_eq!(
        type_of(sample.media),
        libvlc_media_type_t_libvlc_media_type_file,
        "a local file is not typed as one"
    );
    println!("a media from a path: mrl {mrl}, type file");

    // A location is stored as it came in: no normalisation and no rewriting.
    let location = CString::new("http://example.invalid/movie.mp4").unwrap();
    let remote = unsafe { libvlc_media_new_location(location.as_ptr()) };
    assert!(!remote.is_null(), "libvlc_media_new_location returned NULL");
    assert_eq!(
        mrl_of(remote),
        "http://example.invalid/movie.mp4",
        "a media built from a location did not report that location"
    );
    assert_eq!(
        type_of(remote),
        libvlc_media_type_t_libvlc_media_type_file,
        "http is a file to libvlc, not a stream"
    );
    println!("a media from a location: mrl {location:?}, type file");
    unsafe { libvlc_media_release(remote) };
}

/// A local playlist is a file until it is parsed, and a playlist afterwards.
///
/// The type is not decided once and for all: libvlc guesses it from the MRL's
/// scheme when the media is built, and the demuxer's own answer replaces it once
/// the media has been parsed. That is the difference between "what scheme is this"
/// and "what is in here", and a caller that needs the second answer has to get the
/// parse going -- which for a media whose type libvlc cannot guess at all (the
/// in-memory one behind `load_from_file`) means passing `PARSE_FORCED`.
#[test]
fn a_playlist_is_a_file_until_it_is_parsed_and_a_playlist_after() {
    let uri = playlist_uri();
    let location = CString::new(uri.clone()).expect("the playlist URI contains a NUL byte");
    let media = unsafe { libvlc_media_new_location(location.as_ptr()) };
    assert!(
        !media.is_null(),
        "libvlc_media_new_location refused the playlist"
    );

    assert_eq!(
        type_of(media),
        libvlc_media_type_t_libvlc_media_type_file,
        "a .m3u is guessed from its scheme, and its scheme is file"
    );

    // Playing it is what lets the playlist demuxer answer for the media: the
    // entries it finds become the media's subitems, and its type becomes the
    // demuxer's.
    let sample = Sample::new();
    let after = play_until_typed(
        sample.instance,
        media,
        libvlc_media_type_t_libvlc_media_type_playlist,
    );
    println!("the playlist is typed {after} once it has played");

    assert_eq!(
        after, libvlc_media_type_t_libvlc_media_type_playlist,
        "the parse did not turn the media into what it holds"
    );
    assert_eq!(
        mrl_of(media),
        uri,
        "the MRL changed across the parse, and it does not do that"
    );

    unsafe { libvlc_media_release(media) };
}

/// A media's subitems are its own list: read-only, live, and the same one every time.
///
/// This is what §3.7 deferred to here. The list is not a snapshot: the parsing thread
/// appends to it, which is why reading it means holding its lock -- and that lock is
/// not recursive, so a caller that is inside one of its event callbacks must not take
/// it again.
#[test]
fn a_parsed_playlist_holds_its_entries_as_read_only_subitems() {
    let uri = playlist_uri();
    let location = CString::new(uri.clone()).expect("the playlist URI contains a NUL byte");
    let media = unsafe { libvlc_media_new_location(location.as_ptr()) };
    assert!(
        !media.is_null(),
        "libvlc_media_new_location refused the playlist"
    );

    let sample = Sample::new();
    let typed = play_until_typed(
        sample.instance,
        media,
        libvlc_media_type_t_libvlc_media_type_playlist,
    );
    assert_eq!(
        typed, libvlc_media_type_t_libvlc_media_type_playlist,
        "the playlist was not parsed, so it holds nothing to look at"
    );

    let subitems = unsafe { libvlc_media_subitems(media) };
    assert!(
        !subitems.is_null(),
        "libvlc_media_subitems answered NULL for a media this test built; its header allows \
         that, and its implementation cannot do it"
    );
    // The entries are appended by the parsing thread as it finds them, and measured on CI:
    // they can still be missing at the moment the media is already typed as a playlist.
    // Playing it is what makes libvlc parse it at all, but the type is the demuxer's first
    // word, not its last -- so this waits for the list to hold something rather than reading
    // it the instant the type changes. Locally the entry was always there by then, which is
    // how a test like this passes for a month and then fails on a slower machine.
    let deadline = Instant::now() + PLAYBACK_TIMEOUT;
    let mut count = 0;
    while Instant::now() < deadline {
        unsafe { libvlc_media_list_lock(subitems) };
        count = unsafe { libvlc_media_list_count(subitems) };
        unsafe { libvlc_media_list_unlock(subitems) };
        if count > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    unsafe { libvlc_media_list_lock(subitems) };
    let first = unsafe { libvlc_media_list_item_at_index(subitems, 0) };
    let read_only = unsafe { libvlc_media_list_is_readonly(subitems) };
    unsafe { libvlc_media_list_unlock(subitems) };

    assert_eq!(
        count, 1,
        "the fixture names exactly one entry, and the list holds {count} after waiting for it"
    );
    assert!(
        read_only,
        "a media's subitems answered that they are writable; libvlc marks them read-only"
    );
    let first_mrl = mrl_of(first);
    println!("the playlist holds {count} subitem(s), the first being {first_mrl}");
    assert!(
        first_mrl.contains("h264_64x64_1s"),
        "the entry is {first_mrl}, not the sample the fixture names"
    );

    // Writing to it is refused, and that is the only answer a caller gets: libvlc
    // writes a line to its log and returns -1.
    let extra = unsafe {
        let path = CString::new(
            sample_path()
                .to_str()
                .expect("the sample path is not valid UTF-8"),
        )
        .expect("the sample path contains a NUL byte");
        libvlc_media_new_path(path.as_ptr())
    };
    assert_eq!(
        unsafe { libvlc_media_list_add_media(subitems, extra) },
        -1,
        "a read-only list accepted a media"
    );
    unsafe { libvlc_media_release(extra) };

    // And every call answers the same list: it is the media's own, retained for the
    // caller rather than copied.
    let again = unsafe { libvlc_media_subitems(media) };
    assert_eq!(
        again, subitems,
        "libvlc_media_subitems answered a different list the second time"
    );
    unsafe {
        libvlc_media_list_release(again);
        libvlc_media_list_release(subitems);
        libvlc_media_release(first);
        libvlc_media_release(media);
    }
}

/// A list a script builds takes media, gives them back, and lets go of them.
#[test]
fn a_media_list_takes_and_removes_media() {
    let list = unsafe { libvlc_media_list_new() };
    assert!(!list.is_null(), "libvlc_media_list_new returned NULL");
    assert!(
        !unsafe { libvlc_media_list_is_readonly(list) },
        "a list created by hand answered that it is read-only"
    );

    let first = Sample::new();
    let second = Sample::from_path(path_of(SECOND_SAMPLE));

    unsafe {
        assert_eq!(
            libvlc_media_list_add_media(list, first.media),
            0,
            "the first media was refused"
        );
        assert_eq!(
            libvlc_media_list_add_media(list, second.media),
            0,
            "the second media was refused"
        );
    }
    {
        // Reading is what needs the lock, and this is where a caller has to hold it.
        unsafe { libvlc_media_list_lock(list) };
        assert_eq!(
            unsafe { libvlc_media_list_count(list) },
            2,
            "the list lost a media"
        );
        assert_eq!(
            unsafe { libvlc_media_list_index_of_item(list, second.media) },
            1,
            "the second media is not where it was added"
        );
        assert_eq!(
            unsafe { libvlc_media_list_index_of_item(list, first.media) },
            0,
            "the first media is not where it was added"
        );
        unsafe { libvlc_media_list_unlock(list) };
    }

    // An item taken out is a reference of the caller's, and it is the same media.
    let item = unsafe { libvlc_media_list_item_at_index(list, 1) };
    assert!(!item.is_null(), "the second index answered no media");
    assert_eq!(
        mrl_of(item),
        mrl_of(second.media),
        "the wrong media came back"
    );
    unsafe { libvlc_media_release(item) };

    assert!(
        unsafe { libvlc_media_list_item_at_index(list, 2) }.is_null(),
        "an index past the end answered a media"
    );

    // Inserting puts a media where it is asked to, and removing takes it away.
    unsafe {
        assert_eq!(
            libvlc_media_list_insert_media(list, second.media, 0),
            0,
            "the insert was refused"
        );
    }
    {
        unsafe { libvlc_media_list_lock(list) };
        assert_eq!(
            unsafe { libvlc_media_list_count(list) },
            3,
            "the insert did not take"
        );
        assert_eq!(
            unsafe { libvlc_media_list_index_of_item(list, second.media) },
            0,
            "the inserted media is not at the front"
        );
        unsafe { libvlc_media_list_unlock(list) };
    }
    unsafe {
        assert_eq!(
            libvlc_media_list_remove_index(list, 0),
            0,
            "the removal was refused"
        );
        assert_eq!(
            libvlc_media_list_remove_index(list, 99),
            -1,
            "removing an index past the end was accepted"
        );
    }
    {
        unsafe { libvlc_media_list_lock(list) };
        assert_eq!(
            unsafe { libvlc_media_list_count(list) },
            2,
            "the removal did not take"
        );
        unsafe { libvlc_media_list_unlock(list) };
    }

    unsafe { libvlc_media_list_release(list) };
}

/// What a list event reported, for the probe below.
#[derive(Debug, PartialEq)]
enum RecordedListEvent {
    WillAdd(*mut libvlc_media_t, i32),
    Added(*mut libvlc_media_t, i32),
    WillDelete(*mut libvlc_media_t, i32),
    Deleted(*mut libvlc_media_t, i32),
    EndReached,
}

/// Collects what a list's events reported.
///
/// Nothing else may happen in these callbacks: libvlc sends them from inside the
/// modification that caused them, with the list's own lock held, and that lock is not
/// recursive.
#[derive(Default)]
struct ListEventProbe {
    events: Mutex<Vec<RecordedListEvent>>,
}

impl ListEventProbe {
    fn take(&self) -> Vec<RecordedListEvent> {
        std::mem::take(&mut *self.events.lock().expect("the probe lock was poisoned"))
    }

    /// Whether the parse's end has been reported yet.
    fn saw_end(&self) -> bool {
        self.events
            .lock()
            .expect("the probe lock was poisoned")
            .contains(&RecordedListEvent::EndReached)
    }
}

unsafe extern "C" fn probe_will_add(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListEventProbe);
        let payload = (*event).u.media_list_will_add_item;
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListEvent::WillAdd(payload.item, payload.index));
    }
}

unsafe extern "C" fn probe_added(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListEventProbe);
        let payload = (*event).u.media_list_item_added;
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListEvent::Added(payload.item, payload.index));
    }
}

unsafe extern "C" fn probe_will_delete(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListEventProbe);
        let payload = (*event).u.media_list_will_delete_item;
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListEvent::WillDelete(payload.item, payload.index));
    }
}

unsafe extern "C" fn probe_deleted(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListEventProbe);
        let payload = (*event).u.media_list_item_deleted;
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListEvent::Deleted(payload.item, payload.index));
    }
}

unsafe extern "C" fn probe_end_reached(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListEventProbe);
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListEvent::EndReached);
    }
}

/// Attaches the probe to a list's five events.
fn watch_list_events(list: *mut libvlc_media_list_t, probe: &mut ListEventProbe) {
    unsafe {
        let manager = libvlc_media_list_event_manager(list);
        let data = probe as *mut ListEventProbe as *mut c_void;
        for (event_type, callback) in [
            (
                libvlc_event_e_libvlc_MediaListWillAddItem,
                probe_will_add as unsafe extern "C" fn(_, _),
            ),
            (libvlc_event_e_libvlc_MediaListItemAdded, probe_added),
            (
                libvlc_event_e_libvlc_MediaListWillDeleteItem,
                probe_will_delete,
            ),
            (libvlc_event_e_libvlc_MediaListItemDeleted, probe_deleted),
            (libvlc_event_e_libvlc_MediaListEndReached, probe_end_reached),
        ] {
            libvlc_event_attach(
                manager,
                event_type as libvlc_event_type_t,
                Some(callback),
                data,
            );
        }
    }
}

/// The list events carry the media and the index, and a parse ends with its own event.
///
/// The first half is a list this test writes to: libvlc reports each change twice, once
/// before it happens and once after, which is why the binding exposes four item
/// signals rather than two. The second half is a media's subitems list during a parse
/// -- the live case -- and it ends with `MediaListEndReached`, whose payload libvlc
/// never writes and which the binding therefore reports with no arguments at all.
#[test]
fn the_list_events_report_what_moved_and_where() {
    let list = unsafe { libvlc_media_list_new() };
    let sample = Sample::new();
    let mut probe = ListEventProbe::default();
    watch_list_events(list, &mut probe);

    unsafe {
        assert_eq!(
            libvlc_media_list_add_media(list, sample.media),
            0,
            "the media was refused"
        );
    }
    let added = probe.take();
    println!("adding a media reported {added:?}");
    assert_eq!(
        added,
        vec![
            RecordedListEvent::WillAdd(sample.media, 0),
            RecordedListEvent::Added(sample.media, 0),
        ],
        "adding a media did not report the pair of events with the media and its index"
    );

    unsafe {
        assert_eq!(
            libvlc_media_list_remove_index(list, 0),
            0,
            "the removal was refused"
        );
    }
    let removed = probe.take();
    println!("removing a media reported {removed:?}");
    assert_eq!(
        removed,
        vec![
            RecordedListEvent::WillDelete(sample.media, 0),
            RecordedListEvent::Deleted(sample.media, 0),
        ],
        "removing a media did not report the pair of events with the media and its index"
    );
    unsafe { libvlc_media_list_release(list) };

    // The live case: a playlist's subitems while a parse fills them.
    //
    // A **parse** is what reports the end, not playback. `libvlc_MediaListEndReached`
    // is sent from the one place libvlc reports a media's parsed status changing
    // (`send_parsed_changed`, `lib/media.c:296-300`), and playing a media goes through
    // the input instead: the entries still arrive -- measured above, as `WillAdd` and
    // `Added` -- but the end never does. That is why this asks for the parse directly.
    let uri = playlist_uri();
    let location = CString::new(uri).expect("the playlist URI contains a NUL byte");
    let playlist = unsafe { libvlc_media_new_location(location.as_ptr()) };
    let subitems = unsafe { libvlc_media_subitems(playlist) };
    let mut parse_probe = ListEventProbe::default();
    watch_list_events(subitems, &mut parse_probe);

    assert_eq!(
        unsafe {
            libvlc_media_parse_request(
                sample.instance,
                playlist,
                libvlc_media_parse_flag_t_libvlc_media_parse_local
                    | libvlc_media_parse_flag_t_libvlc_media_parse_forced,
                0,
            )
        },
        0,
        "the parse was refused"
    );
    let deadline = Instant::now() + PLAYBACK_TIMEOUT;
    while Instant::now() < deadline && !parse_probe.saw_end() {
        std::thread::sleep(Duration::from_millis(10));
    }

    let filled = parse_probe.take();
    println!("parsing the playlist reported {filled:?}");
    assert!(
        filled.contains(&RecordedListEvent::EndReached),
        "the end of the parse was never reported: {filled:?}"
    );
    let first_added = filled
        .iter()
        .position(|event| matches!(event, RecordedListEvent::Added(_, _)));
    let end = filled
        .iter()
        .position(|event| matches!(event, RecordedListEvent::EndReached));
    assert!(
        first_added.is_some() && first_added < end,
        "the entries and the end of the parse are out of order: {filled:?}"
    );

    unsafe {
        libvlc_media_list_release(subitems);
        libvlc_media_release(playlist);
    }
}

/// What a list player's events reported, for the probes below.
#[derive(Debug, PartialEq)]
enum RecordedListPlayerEvent {
    Played,
    NextItemSet(*mut libvlc_media_t),
    Stopped,
}

/// Collects what a list player's events reported.
#[derive(Default)]
struct ListPlayerProbe {
    events: Mutex<Vec<RecordedListPlayerEvent>>,
}

impl ListPlayerProbe {
    fn take(&self) -> Vec<RecordedListPlayerEvent> {
        std::mem::take(&mut *self.events.lock().expect("the probe lock was poisoned"))
    }

    fn next_items(&self) -> Vec<*mut libvlc_media_t> {
        self.events
            .lock()
            .expect("the probe lock was poisoned")
            .iter()
            .filter_map(|event| match event {
                RecordedListPlayerEvent::NextItemSet(media) => Some(*media),
                _ => None,
            })
            .collect()
    }

    fn saw_stopped(&self) -> bool {
        self.events
            .lock()
            .expect("the probe lock was poisoned")
            .contains(&RecordedListPlayerEvent::Stopped)
    }
}

unsafe extern "C" fn probe_list_player_played(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListPlayerProbe);
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListPlayerEvent::Played);
    }
}

unsafe extern "C" fn probe_next_item_set(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListPlayerProbe);
        let media = (*event).u.media_list_player_next_item_set.item;
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListPlayerEvent::NextItemSet(media));
    }
}

unsafe extern "C" fn probe_list_player_stopped(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ListPlayerProbe);
        probe
            .events
            .lock()
            .expect("the probe lock was poisoned")
            .push(RecordedListPlayerEvent::Stopped);
    }
}

/// Attaches the probe to a list player's three events.
fn watch_list_player(list_player: *mut libvlc_media_list_player_t, probe: &mut ListPlayerProbe) {
    unsafe {
        let manager = libvlc_media_list_player_event_manager(list_player);
        let data = probe as *mut ListPlayerProbe as *mut c_void;
        for (event_type, callback) in [
            (
                libvlc_event_e_libvlc_MediaListPlayerPlayed,
                probe_list_player_played as unsafe extern "C" fn(_, _),
            ),
            (
                libvlc_event_e_libvlc_MediaListPlayerNextItemSet,
                probe_next_item_set,
            ),
            (
                libvlc_event_e_libvlc_MediaListPlayerStopped,
                probe_list_player_stopped,
            ),
        ] {
            libvlc_event_attach(
                manager,
                event_type as libvlc_event_type_t,
                Some(callback),
                data,
            );
        }
    }
}

/// Waits until the probe has been told about `wanted` items, or `timeout` passes.
fn wait_for_next_items(probe: &ListPlayerProbe, wanted: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe.next_items().len() >= wanted {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// A list plays through the player it is given, and moves on by itself.
///
/// The list holds two different media, so the two items can be told apart by their
/// announcements: libvlc emits `MediaListPlayerNextItemSet` for the item it is about to
/// play, and the announcement changing from the first media to the second means the
/// first ended and the list moved on -- which is what a list player is for, and what
/// nothing else in libvlc does for a caller.
#[test]
fn a_list_player_plays_through_its_player_and_moves_on() {
    let sample = Sample::new();
    let second = Sample::from_path(path_of(SECOND_SAMPLE));
    let list = unsafe { libvlc_media_list_new() };
    unsafe {
        assert_eq!(
            libvlc_media_list_add_media(list, sample.media),
            0,
            "the first item was refused"
        );
        assert_eq!(
            libvlc_media_list_add_media(list, second.media),
            0,
            "the second item was refused"
        );
    }

    let list_player = unsafe { libvlc_media_list_player_new(sample.instance) };
    assert!(
        !list_player.is_null(),
        "libvlc_media_list_player_new returned NULL"
    );
    let mut probe = ListPlayerProbe::default();
    watch_list_player(list_player, &mut probe);

    unsafe {
        libvlc_media_list_player_set_media_player(list_player, sample.player);
        libvlc_media_list_player_set_media_list(list_player, list);
        libvlc_media_list_player_play(list_player);
    }
    assert!(
        wait_for_next_items(&probe, 1, PLAYBACK_TIMEOUT),
        "playing the list announced no item"
    );
    let announced = probe.next_items();
    assert_eq!(
        announced[0], sample.media,
        "the first announced item is not the media the list holds"
    );
    println!("the list announced its first item; waiting for the second");

    // The item is one second long, so the list has to move on by itself.
    let advanced = wait_for_next_items(&probe, 2, PLAYBACK_TIMEOUT);
    let announced = probe.next_items();
    assert!(
        advanced,
        "the list did not move to its second item within {PLAYBACK_TIMEOUT:?}: {announced:?}"
    );
    assert_eq!(
        announced[1], second.media,
        "the second announced item is not the second media the list holds"
    );

    // Its own stop is reported, and that report is the only source of it: stopping the
    // player underneath would be taken for the item ending.
    unsafe { libvlc_media_list_player_stop_async(list_player) };
    let deadline = Instant::now() + PLAYBACK_TIMEOUT;
    while Instant::now() < deadline && !probe.saw_stopped() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let events = probe.take();
    println!("the list player reported {events:?}");
    assert!(
        events.contains(&RecordedListPlayerEvent::Stopped),
        "the list player's own stop was never reported: {events:?}"
    );

    unsafe {
        libvlc_media_list_player_release(list_player);
        libvlc_media_list_release(list);
    }
}

/// Stopping the underlying player directly moves the list on, and repeat mode does not
/// move at all.
///
/// Both are traps a caller has to be told about, and both are measured here rather than
/// reasoned about. The first is why the binding documents that a list player's own
/// `stop_async` is the one to use: libvlc's list player listens for the player stopping
/// to learn that an item ended, and cannot tell the two apart. The second is libvlc's
/// repeat mode, which replays the current item and leaves `next` with nowhere to go.
#[test]
fn stopping_the_player_directly_moves_the_list_on() {
    let sample = Sample::new();
    let second = Sample::from_path(path_of(SECOND_SAMPLE));
    let list = unsafe { libvlc_media_list_new() };
    unsafe {
        libvlc_media_list_add_media(list, sample.media);
        libvlc_media_list_add_media(list, second.media);
    }
    let list_player = unsafe { libvlc_media_list_player_new(sample.instance) };
    let mut probe = ListPlayerProbe::default();
    watch_list_player(list_player, &mut probe);
    unsafe {
        libvlc_media_list_player_set_media_player(list_player, sample.player);
        libvlc_media_list_player_set_media_list(list_player, list);
        libvlc_media_list_player_play(list_player);
    }
    assert!(
        wait_for_next_items(&probe, 1, PLAYBACK_TIMEOUT),
        "playing the list announced no item"
    );

    // The player, not the list player: the list takes this for "the item ended".
    unsafe { libvlc_media_player_stop_async(sample.player) };
    let advanced = wait_for_next_items(&probe, 2, PLAYBACK_TIMEOUT);
    let announced = probe.next_items();
    println!(
        "stopping the player under the list announced {} item(s)",
        announced.len()
    );
    assert!(
        advanced,
        "stopping the player directly did not move the list on: {announced:?}"
    );
    assert_eq!(
        announced[1], second.media,
        "the list moved somewhere other than its second item"
    );

    unsafe {
        libvlc_media_list_player_release(list_player);
        libvlc_media_list_release(list);
    }
}

/// Under repeat, `next` replays the current item instead of moving to the next one.
///
/// libvlc's repeat branch does not look for a next item at all: it plays the path it is
/// already on (`set_relative_playlist_position_and_play`, `lib/media_list_player.c:777-790`).
/// In loop mode the list starts again at the first item when it runs out.
///
/// The list holds the one-second sample twice, so a wrap is the third announcement: the
/// first two are the items, and the third can only be the list starting over. Repeat
/// mode is the other way libvlc spells this, and it is deliberately **not** tested here
/// because it aborts the runtime -- measured, and documented on
/// `PLAYBACK_MODE_REPEAT` in the binding, which refuses the value.
#[test]
fn loop_mode_wraps_to_the_first_item() {
    let sample = Sample::new();
    let list = unsafe { libvlc_media_list_new() };
    unsafe {
        libvlc_media_list_add_media(list, sample.media);
        libvlc_media_list_add_media(list, sample.media);
    }
    let list_player = unsafe { libvlc_media_list_player_new(sample.instance) };
    let mut probe = ListPlayerProbe::default();
    watch_list_player(list_player, &mut probe);
    unsafe {
        libvlc_media_list_player_set_media_player(list_player, sample.player);
        libvlc_media_list_player_set_media_list(list_player, list);
        libvlc_media_list_player_set_playback_mode(
            list_player,
            libvlc_playback_mode_t_libvlc_playback_mode_loop,
        );
        libvlc_media_list_player_play(list_player);
    }

    // Two one-second items, then the wrap: three announcements, unless the list stops
    // after the second one, which is what the default mode does.
    let wrapped = wait_for_next_items(&probe, 3, PLAYBACK_TIMEOUT);
    let announced = probe.next_items();
    println!(
        "in loop mode the list announced {} item(s)",
        announced.len()
    );
    assert!(
        wrapped,
        "the list announced {} item(s) and then stopped, so it did not loop",
        announced.len()
    );
    for item in &announced {
        assert_eq!(
            *item, sample.media,
            "the loop announced a media that is not in the list"
        );
    }

    unsafe {
        libvlc_media_list_player_stop_async(list_player);
        libvlc_media_list_player_release(list_player);
        libvlc_media_list_release(list);
    }
}

/// A duplicate is a media of its own: what is added to one is not on the other.
///
/// `libvlc_media_duplicate` copies the MRL, the metadata, the options, the slaves
/// and the tracks, and its header promises that the copy "won't share forthcoming
/// updates from the original". This measures that instead of trusting it: the copy
/// is given a `:start-time` the original never saw, and the two are played in turn.
#[test]
fn duplicating_a_media_copies_it_and_leaves_the_original_alone() {
    let sample = Sample::new();
    let copy = unsafe { libvlc_media_duplicate(sample.media) };
    assert!(!copy.is_null(), "libvlc_media_duplicate returned NULL");

    assert_eq!(
        mrl_of(copy),
        mrl_of(sample.media),
        "the copy does not point where the original does"
    );

    let option = CString::new(format!(":start-time={START_TIME_SECS}")).unwrap();
    unsafe { libvlc_media_add_option(copy, option.as_ptr()) };

    let shortened = reported_length(sample.instance, copy);
    let untouched = reported_length(sample.instance, sample.media);
    println!(
        "duplicate: the copy reports {shortened} ms with :start-time={START_TIME_SECS}, the original {untouched} ms"
    );

    assert!(
        shortened <= SHORTENED_LENGTH_MAX_MS,
        "the option added to the copy did not take effect on it: {shortened} ms"
    );
    assert!(
        untouched >= FULL_LENGTH_MIN_MS,
        "the original was shortened by an option added to its copy: {untouched} ms"
    );

    unsafe { libvlc_media_release(copy) };
}

/// The commit `build/vlc/vlc.lock` pins, read from the lock rather than written down
/// here so that a re-pin moves the assertion with it.
fn pinned_vlc_commit() -> String {
    let lock = std::fs::read_to_string(path_of("build/vlc/vlc.lock"))
        .expect("build/vlc/vlc.lock could not be read");
    lock.lines()
        .find_map(|line| line.strip_prefix("VLC_COMMIT="))
        .expect("build/vlc/vlc.lock has no VLC_COMMIT line")
        .trim()
        .to_string()
}

/// What libvlc recorded for the last failure on this thread, or `""` for none.
fn error_message() -> String {
    unsafe { c_string(libvlc_errmsg()) }
}

/// The runtime answers which LibVLC it is, and the answer names the pinned revision.
///
/// This is the runtime half of a question the repository otherwise answers only at
/// build time: `build/vlc/vlc.lock` pins the revision, `build-info.txt` records it
/// inside the artifact, and `check_vlc_provenance.ps1` compares the platforms -- none
/// of which a running game can read. Both strings are compile-time constants of the
/// runtime, so nothing here is freed or waited for.
#[test]
fn the_runtime_reports_which_libvlc_it_is() {
    let version = unsafe { CStr::from_ptr(libvlc_get_version()) }
        .to_str()
        .expect("the version string is not UTF-8")
        .to_string();
    let changeset = unsafe { CStr::from_ptr(libvlc_get_changeset()) }
        .to_str()
        .expect("the changeset string is not UTF-8")
        .to_string();
    let pinned = pinned_vlc_commit();
    println!("the runtime is libvlc {version}, changeset {changeset}; vlc.lock pins {pinned}");

    assert!(
        version.contains("4.0"),
        "the version string is {version:?}, which does not name the 4.0 runtime this addon ships"
    );
    // A `git describe` value, not a bare hash: the abbreviated commit is in it, and
    // that is what makes it comparable to the pin.
    assert!(
        changeset.contains(&pinned),
        "the changeset {changeset:?} does not name the pinned commit {pinned:?}"
    );
}

/// The error status belongs to the thread that failed, and success does not clear it.
///
/// This is the contract the binding's failure logs rest on. `libvlc_errmsg` keeps one
/// message per thread, a call that succeeds leaves it alone, and the only way to be
/// sure an answer belongs to *your* call is to clear it first -- which is what
/// `clear_last_error` exists for.
///
/// The ordering in this test is part of what it pins, and it was measured the hard
/// way: asking for the message **before any instance exists** does not answer NULL,
/// it crashes. libvlc creates the thread-local that holds the message while it
/// initialises, so those two calls are only safe once `libvlc_new` has returned --
/// which in this extension is always, since the instance is built at `Scene` stage
/// before any script can run.
#[test]
fn the_error_status_is_cleared_by_hand_and_not_by_success() {
    let sample = Sample::new();
    assert!(!sample.media.is_null(), "the sample media is null");

    unsafe { libvlc_clearerr() };
    let after_clear = error_message();
    assert!(
        after_clear.is_empty(),
        "libvlc_clearerr left {after_clear:?} behind"
    );

    // A call that succeeds leaves it alone rather than filling it in.
    let path = CString::new(
        sample_path()
            .to_str()
            .expect("the sample path is not valid UTF-8"),
    )
    .expect("the sample path contains a NUL byte");
    let another = unsafe { libvlc_media_new_path(path.as_ptr()) };
    assert!(
        !another.is_null(),
        "libvlc_media_new_path refused the sample"
    );
    let after_success = error_message();
    assert!(
        after_success.is_empty(),
        "a successful call recorded {after_success:?} as an error"
    );
    unsafe { libvlc_media_release(another) };
}

/// libvlc's log context names the module, the source file and the line.
///
/// The binding puts all three into the console line, so this pins what libvlc hands
/// over: the module name with its directory and extension already stripped, the
/// emitter's own `__FILE__`, and a line number. The callback here is this test's own,
/// because it is the runtime's half that is being measured -- the binding's half is
/// `demo/tests/log_message.gd`.
/// One log line as the runtime logged it, with everything the context carried.
#[derive(Clone, Debug)]
struct RecordedLog {
    level: i32,
    module: String,
    file: String,
    line: u32,
    message: String,
}

/// Where the log callback of the test below leaves what it received.
struct LogProbe {
    lines: Mutex<Vec<RecordedLog>>,
}

impl LogProbe {
    fn new() -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> Vec<RecordedLog> {
        match self.lines.lock() {
            Ok(lines) => lines.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[test]
fn the_log_context_names_the_module_and_where_it_came_from() {
    /// Records one line, copying everything out of the context before it goes away.
    unsafe extern "C" fn record(
        data: *mut c_void,
        level: c_int,
        ctx: *const libvlc_log_t,
        fmt: *const c_char,
        args: *mut c_void,
    ) {
        unsafe {
            let probe = &*(data as *const LogProbe);
            let mut module: *const c_char = ptr::null();
            let mut file: *const c_char = ptr::null();
            let mut line: c_uint = 0;
            libvlc_log_get_context(ctx, &mut module, &mut file, &mut line);
            let recorded = RecordedLog {
                level,
                module: c_string(module),
                file: c_string(file),
                line,
                message: printf(fmt, args),
            };
            let mut lines = match probe.lines.lock() {
                Ok(lines) => lines,
                Err(poisoned) => poisoned.into_inner(),
            };
            lines.push(recorded);
        }
    }

    let mut probe = Box::new(LogProbe::new());
    let opaque = probe.as_mut() as *mut LogProbe as *mut c_void;
    let sample = Sample::new();
    // The same transmute the binding does, and for the same reason: bindgen types
    // the callback's `va_list` differently per target, so the function pointer
    // cannot be passed as it stands everywhere.
    #[allow(clippy::missing_transmute_annotations)]
    let cb = unsafe { Some(std::mem::transmute(record as *const ())) };
    unsafe { libvlc_log_set(sample.instance, cb, opaque) };

    // A media that is not there makes the access module speak up: the line is the
    // runtime's own report of the failure, and the reason nothing else reports.
    let missing = path_of(MISSING_MEDIA);
    assert!(
        !missing.exists(),
        "the test needs {} to be absent, and something created it",
        missing.display()
    );
    let missing = CString::new(missing.to_str().expect("the path is not UTF-8"))
        .expect("the path contains a NUL byte");
    let media = unsafe { libvlc_media_new_path(missing.as_ptr()) };
    assert!(!media.is_null(), "libvlc_media_new_path refused the path");
    unsafe {
        libvlc_media_player_set_media(sample.player, media);
    }
    // Not `play_until_playing`: the media is not there, so the player never reaches
    // Playing. `play` accepts it and the failure arrives as this log line.
    assert_eq!(
        unsafe { libvlc_media_player_play(sample.player) },
        0,
        "the playback of a missing media was refused rather than reported"
    );

    let deadline = Instant::now() + ERROR_TIMEOUT;
    while Instant::now() < deadline && probe.snapshot().len() < 2 {
        std::thread::sleep(Duration::from_millis(20));
    }
    let lines = probe.snapshot();
    unsafe { libvlc_media_release(media) };
    let levels: Vec<i32> = lines.iter().map(|line| line.level).collect();
    println!(
        "the runtime logged {} lines, at levels {levels:?}",
        lines.len()
    );
    println!("the runtime logged: {lines:#?}");

    assert!(
        lines.iter().any(|line| !line.module.is_empty()),
        "no line named the module that logged it: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| !line.file.is_empty() && line.line > 0),
        "no line named the source file and line it came from: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.message.contains("this-media-does-not-exist")),
        "no line mentioned the media that could not be opened, so these are not the \
         lines this test is about: {lines:?}"
    );
}

/// Collects what a thumbnail request reported, keeping a reference on each picture.
///
/// The payload is borrowed -- libvlc releases it as soon as the event has been delivered --
/// so the probe retains, exactly as the binding does.
#[derive(Default)]
struct ThumbnailProbe {
    pictures: Mutex<Vec<*mut libvlc_picture_t>>,
}

impl ThumbnailProbe {
    fn take(&self) -> Vec<*mut libvlc_picture_t> {
        std::mem::take(&mut *self.pictures.lock().expect("the probe lock was poisoned"))
    }
}

unsafe extern "C" fn probe_thumbnail_generated(event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        let probe = &*(data as *const ThumbnailProbe);
        let picture = (*event).u.media_thumbnail_generated.p_thumbnail;
        let held = if picture.is_null() {
            std::ptr::null_mut()
        } else {
            libvlc_picture_retain(picture)
        };
        probe
            .pictures
            .lock()
            .expect("the probe lock was poisoned")
            .push(held);
    }
}

/// A thumbnail request answers with a picture, and its stride describes its buffer.
///
/// Two of the three measurements this section was opened with, in one run: whether the
/// shipped runtime can produce a picture for each type at all -- ARGB and RGBA go through
/// libvlc's raw-video encoder, which is a plugin that may or may not be there, and a missing
/// one is reported as a NULL payload rather than an error -- and whether
/// `libvlc_picture_get_stride`'s `width * 4` is the truth about the buffer, which the header
/// does not promise: it describes the buffer as including "potential padding" while giving no
/// way to ask how much.
///
/// The printout is the point of the test as much as the assertions are: it records what this
/// runtime actually does, which is what the documentation of both types quotes.
#[test]
fn a_thumbnail_request_reports_a_picture_and_a_stride_that_matches_its_buffer() {
    let sample = Sample::new();
    for (label, picture_type, stride_is_meaningful) in [
        ("ARGB", libvlc_picture_type_t_libvlc_picture_Argb, true),
        ("RGBA", libvlc_picture_type_t_libvlc_picture_Rgba, true),
        ("PNG", libvlc_picture_type_t_libvlc_picture_Png, false),
    ] {
        let mut probe = ThumbnailProbe::default();
        unsafe {
            libvlc_event_attach(
                libvlc_media_event_manager(sample.media),
                libvlc_event_e_libvlc_MediaThumbnailGenerated as libvlc_event_type_t,
                Some(probe_thumbnail_generated),
                &mut probe as *mut ThumbnailProbe as *mut c_void,
            );
        }

        // The middle of the media, which is what the editor's inspector asks for: a position
        // needs no known duration, so this works before anything has parsed it.
        let request = unsafe {
            libvlc_media_thumbnail_request_by_pos(
                sample.instance,
                sample.media,
                0.5,
                libvlc_thumbnailer_seek_speed_t_libvlc_media_thumbnail_seek_precise,
                256,
                256,
                false,
                picture_type,
                5000,
            )
        };
        assert!(!request.is_null(), "{label}: libvlc refused the request");

        let deadline = Instant::now() + PLAYBACK_TIMEOUT;
        while Instant::now() < deadline && probe.pictures.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let reported = probe.take();
        assert!(
            !reported.is_empty(),
            "{label}: no event arrived within {PLAYBACK_TIMEOUT:?}, so the request was neither \
             answered nor refused"
        );
        let picture = reported[0];
        if picture.is_null() {
            println!("{label}: this runtime produced no picture (a NULL payload)");
        } else {
            let mut size: usize = 0;
            let buffer = unsafe { libvlc_picture_get_buffer(picture, &mut size) };
            let width = unsafe { libvlc_picture_get_width(picture) };
            let height = unsafe { libvlc_picture_get_height(picture) };
            let picture_type_reported = unsafe { libvlc_picture_type(picture) };
            println!(
                "{label}: {width}x{height}, buffer {size} bytes, reported type {picture_type_reported}"
            );
            assert!(!buffer.is_null() && size > 0, "{label}: no buffer");
            if stride_is_meaningful {
                let stride = unsafe { libvlc_picture_get_stride(picture) };
                println!("{label}: stride {stride}");
                assert_eq!(
                    size,
                    stride as usize * height as usize,
                    "{label}: the buffer is {size} bytes but stride * height is {}, so the rows \
                     are padded and the stride does not describe the buffer",
                    stride as usize * height as usize
                );
            }
            unsafe { libvlc_picture_release(picture) };
        }

        unsafe { libvlc_media_thumbnail_request_destroy(request) };
    }
}

/// One picture from one request, retained for the caller, or null when there was none.
///
/// Requests are made one at a time and waited for one at a time: with two outstanding, an
/// arrival cannot be attributed to the request that caused it.
fn one_thumbnail_picture(
    instance: *mut libvlc_instance_t,
    media: *mut libvlc_media_t,
    picture_type: libvlc_picture_type_t,
    size: u32,
) -> *mut libvlc_picture_t {
    let mut probe = ThumbnailProbe::default();
    unsafe {
        libvlc_event_attach(
            libvlc_media_event_manager(media),
            libvlc_event_e_libvlc_MediaThumbnailGenerated as libvlc_event_type_t,
            Some(probe_thumbnail_generated),
            &mut probe as *mut ThumbnailProbe as *mut c_void,
        );
    }
    let request = unsafe {
        libvlc_media_thumbnail_request_by_pos(
            instance,
            media,
            0.5,
            libvlc_thumbnailer_seek_speed_t_libvlc_media_thumbnail_seek_precise,
            size,
            size,
            false,
            picture_type,
            5000,
        )
    };
    assert!(!request.is_null(), "libvlc refused the request");
    let deadline = Instant::now() + PLAYBACK_TIMEOUT;
    while Instant::now() < deadline && probe.pictures.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let reported = probe.take();
    assert!(!reported.is_empty(), "no event arrived");
    unsafe { libvlc_media_thumbnail_request_destroy(request) };
    reported[0]
}

/// What byte order an ARGB buffer actually holds.
///
/// libvlc's own name for the type says alpha, red, green, blue, and Godot wants red, green,
/// blue, alpha, so `to_image` has to rotate every pixel -- but the order a buffer really holds
/// depends on the encoder and the platform, and the header does not pin it down. So this takes
/// the same frame twice, in both raw types, and compares them: if rotating ARGB's four bytes
/// left by one reproduces RGBA exactly, the documented order is what this runtime writes.
///
/// The printout is what the conversion is written from, whichever way it goes.
#[test]
fn the_argb_buffer_is_the_rgba_buffer_rotated() {
    let sample = Sample::new();
    let argb = one_thumbnail_picture(
        sample.instance,
        sample.media,
        libvlc_picture_type_t_libvlc_picture_Argb,
        64,
    );
    let rgba = one_thumbnail_picture(
        sample.instance,
        sample.media,
        libvlc_picture_type_t_libvlc_picture_Rgba,
        64,
    );
    assert!(
        !argb.is_null() && !rgba.is_null(),
        "a raw picture was missing"
    );

    let mut argb_size: usize = 0;
    let mut rgba_size: usize = 0;
    let argb_buffer = unsafe { libvlc_picture_get_buffer(argb, &mut argb_size) };
    let rgba_buffer = unsafe { libvlc_picture_get_buffer(rgba, &mut rgba_size) };
    assert_eq!(argb_size, rgba_size, "the two pictures differ in size");
    let argb_bytes = unsafe { std::slice::from_raw_parts(argb_buffer, argb_size) };
    let rgba_bytes = unsafe { std::slice::from_raw_parts(rgba_buffer, rgba_size) };

    let mut rotated: Vec<u8> = Vec::with_capacity(argb_size);
    for index in (0..argb_size).step_by(4) {
        let pixel = &argb_bytes[index..index + 4];
        rotated.extend_from_slice(&[pixel[1], pixel[2], pixel[3], pixel[0]]);
    }
    let matches = rotated == rgba_bytes;
    println!("ARGB rotated left by one byte equals RGBA: {matches}");
    if !matches {
        let first = 8.min(argb_size);
        println!(
            "first bytes -- ARGB {:?}, RGBA {:?}",
            &argb_bytes[..first],
            &rgba_bytes[..first]
        );
    }
    assert!(
        matches,
        "the ARGB buffer is not the RGBA buffer rotated by one byte: the conversion has to be \
         written from the bytes printed above, not from libvlc's name for the type"
    );

    unsafe {
        libvlc_picture_release(argb);
        libvlc_picture_release(rgba);
    }
}

/// Whether the cover-art event arrived at all, for a media that has no cover art.
#[derive(Default)]
struct CoverProbe {
    count: std::sync::atomic::AtomicUsize,
}

impl CoverProbe {
    fn count(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

unsafe extern "C" fn probe_attached_thumbnails(_event: *const libvlc_event_t, data: *mut c_void) {
    unsafe {
        (*(data as *const CoverProbe))
            .count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A media with no embedded cover art never reports one, across as many parses as it takes.
///
/// The interesting word is "never", not "not yet": libvlc's sender returns early when the
/// attachment list is empty (`lib/media.c:251-257`), so there is no event with nothing in it to
/// wait for. That is worth having as a measurement rather than as a reading, because it decides
/// something for the editor's inspector: it cannot wait for this event to find out whether a
/// media has a cover, since for most media the event never comes.
///
/// **The third measurement this section was opened with cannot be taken here.** It asked
/// whether a second `parse_request` reports the covers again; the repository has no fixture with
/// embedded cover art (`test/media` holds one video and a README), so a second parse has nothing
/// to report again. The source says the event is sent from the parse path, which is what the
/// documentation promises; putting a number on it needs a fixture with a cover.
#[test]
fn a_media_without_embedded_cover_art_never_reports_any() {
    let sample = Sample::new();
    let mut probe = CoverProbe::default();
    unsafe {
        libvlc_event_attach(
            libvlc_media_event_manager(sample.media),
            libvlc_event_e_libvlc_MediaAttachedThumbnailsFound as libvlc_event_type_t,
            Some(probe_attached_thumbnails),
            &mut probe as *mut CoverProbe as *mut c_void,
        );
    }

    for round in 1..=2 {
        let parsed = unsafe {
            libvlc_media_parse_request(
                sample.instance,
                sample.media,
                libvlc_media_parse_flag_t_libvlc_media_parse_local
                    | libvlc_media_parse_flag_t_libvlc_media_parse_forced,
                0,
            )
        };
        if round == 2 {
            println!(
                "a second parse_request for the same media answered {parsed}: it is refused, so re-parsing is not a way back to this event"
            );
            break;
        }
        assert_eq!(parsed, 0, "the parse was refused");
        // The absence is what is watched, so there is nothing to wait *for*: a couple of
        // seconds per parse, and the count is read. The source quoted above is why it stays at
        // zero rather than being late.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            probe.count(),
            0,
            "round {round}: a cover-art event arrived for a media with no attachments, which the \
             source says cannot happen"
        );
    }
    println!("two parses of a video with no cover art reported no cover-art event at all");
}
