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
    classes::{AudioServer, AudioStreamPlayer, native::AudioFrame},
    prelude::*,
};
use ringbuf::{HeapProd, traits::Producer};

use super::internal_audio_stream::InternalAudioStream;

/// The node a callback was handed, if it is still there.
///
/// The audio player is a child of the `VLCMediaPlayer`, and Godot destroys a node's
/// children **before** it destroys the extension instance behind the node -- so between
/// those two moments libvlc's audio thread can arrive here holding a `Gd` whose object has
/// been freed. `is_instance_valid` is the one call that answers for such a handle instead
/// of asserting; anything else aborts the process (measured: `AudioStreamPlayer::upcast_ref`,
/// "access to instance ... after it has been freed", in two runs out of three when the
/// demo exits while it is playing).
///
/// Returns the pair the callbacks are given: the ring buffer producer and the node.
unsafe fn audio_context<'a>(
    data: *mut c_void,
) -> Option<(&'a mut HeapProd<AudioFrame>, &'a mut Gd<AudioStreamPlayer>)> {
    let (producer, player) =
        unsafe { (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>)).as_mut()? };
    if !player.is_instance_valid() {
        return None;
    }
    Some((producer, player))
}

pub(super) unsafe extern "C" fn audio_play_callback(
    data: *mut c_void,
    samples: *const c_void,
    count: c_uint,
    _pts: i64,
) {
    unsafe {
        // Nothing is pushed for a node that is gone: the ring buffer has no reader left,
        // and filling it would only report itself full.
        let Some((rb_prod, player)) = audio_context(data) else {
            return;
        };

        let samples_slice = slice_from_raw_parts(samples as *const f32, count as usize * 2)
            .as_ref()
            .unwrap();

        for i in 0..count as usize {
            let left = samples_slice[i * 2];
            let right = samples_slice[i * 2 + 1];
            let frame = AudioFrame { left, right };
            if rb_prod.try_push(frame).is_err() {
                godot_error!("godot-vlc: audio buffer full");
                break;
            }
        }

        if !player.is_playing() {
            player.call_thread_safe("play", &[]);
        }
    }
}

pub(super) unsafe extern "C" fn audio_pause_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        let Some((_, player)) = audio_context(data) else {
            return;
        };
        player.set_stream_paused(true);
    }
}

pub(super) unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        let Some((_, player)) = audio_context(data) else {
            return;
        };
        player.set_stream_paused(false);
    }
}

pub(super) unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        // `audio_context` is the same check this one used to make for itself, and it now
        // covers the whole callback rather than only the call that happened to need it.
        let Some((_, player)) = audio_context(data) else {
            return;
        };
        player.call_thread_safe("stop", &[]);
        if let Some(stream) = player.get_stream()
            && let Ok(mut internal_stream) = stream.try_cast::<InternalAudioStream>()
        {
            internal_stream
                .bind_mut()
                .playback
                .bind_mut()
                .clear_buffer();
        }
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
