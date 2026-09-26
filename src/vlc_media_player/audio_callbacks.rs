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

use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    ptr::slice_from_raw_parts,
};

use godot::{
    classes::{AudioServer, native::AudioFrame},
    prelude::*,
};
use ringbuf::{HeapProd, traits::Producer};

use super::output_sink::OutputSink;

/// Runs one audio callback against the ring buffer it fills, or does nothing when
/// there is nothing left to write into.
///
/// Nothing here reaches the node that plays the buffer back. That node is a child of
/// the `VLCMediaPlayer`, Godot destroys a node's children **before** it destroys the
/// extension instance behind the node, and `Drop` -- the moment the player learns
/// that it is going away -- runs after both. A callback that asks the node whether
/// it is still there is therefore racing the teardown rather than guarding against
/// it, and that was measured, not guessed: the callback passed `is_instance_valid`,
/// the main thread freed the node, and the next call aborted the process with
/// `AudioStreamPlayer::upcast_ref`, "access to instance ... after it has been
/// freed". `is_inside_tree` was the second half of the same attempt and failed the
/// same way. What the callbacks want done to the node is left in the sink instead,
/// and the frame applies it on the main thread
/// (`VlcMediaPlayer::apply_audio_intent`), where the node is only ever reached while
/// the object that owns it is alive.
///
/// The slot is behind a lock because `Drop` takes the ring buffer out of it while
/// these callbacks may be running: the sink is the one thing both sides can reach
/// after the player's own fields are gone.
///
/// # Safety
///
/// `data` has to be an address from `OutputSink::leak`.
unsafe fn with_audio_sink(
    data: *mut c_void,
    run: impl FnOnce(&mut HeapProd<AudioFrame>, &OutputSink),
) {
    let sink = unsafe { OutputSink::from_opaque(data) };
    if sink.is_closed() || !sink.is_accepting() {
        return;
    }
    let Ok(mut slot) = sink.audio().lock() else {
        return;
    };
    let Some(producer) = slot.as_mut() else {
        return;
    };
    run(producer, sink);
}

pub(super) unsafe extern "C" fn audio_play_callback(
    data: *mut c_void,
    samples: *const c_void,
    count: c_uint,
    _pts: i64,
) {
    let samples = unsafe {
        slice_from_raw_parts(samples as *const f32, count as usize * 2)
            .as_ref()
            .unwrap()
    };
    unsafe {
        with_audio_sink(data, |rb_prod, sink| {
            for i in 0..count as usize {
                let left = samples[i * 2];
                let right = samples[i * 2 + 1];
                let frame = AudioFrame { left, right };
                if rb_prod.try_push(frame).is_err() {
                    godot_error!("godot-vlc: audio buffer full");
                    break;
                }
            }

            // The node is started from the frame, not from here: `is_playing` and
            // `play` are both calls on an object this thread does not own.
            sink.request_play();
        });
    }
}

pub(super) unsafe extern "C" fn audio_pause_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        with_audio_sink(data, |_, sink| sink.request_pause(true));
    }
}

pub(super) unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        with_audio_sink(data, |_, sink| sink.request_pause(false));
    }
}

pub(super) unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        // The context is the same check this one used to make for itself, and it now
        // covers the whole callback rather than only the call that happened to need it.
        with_audio_sink(data, |_, sink| sink.request_flush());
    }
}

pub(super) unsafe extern "C" fn audio_drain_callback(_data: *mut c_void) {
    // do nothing
}

pub(super) unsafe extern "C" fn audio_setup_callback(
    _opaque: *mut *mut c_void,
    format: *mut c_char,
    rate: *mut c_uint,
    channels: *mut c_uint,
) -> c_int {
    unsafe {
        format.copy_from(c"FL32".as_ptr(), 5);
        *rate = AudioServer::singleton().get_mix_rate() as c_uint;
        *channels = 2;
        0
    }
}

pub(super) unsafe extern "C" fn audio_cleanup_callback(_opaque: *mut c_void) {
    // do nothing
}
