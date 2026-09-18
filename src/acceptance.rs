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
    let options = [c"--no-audio"];
    let instance = unsafe { libvlc_new(options.len() as i32, options.as_ptr().cast()) };
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
    let options = [c"--no-audio"];
    let instance = unsafe { libvlc_new(options.len() as i32, options.as_ptr().cast()) };
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
