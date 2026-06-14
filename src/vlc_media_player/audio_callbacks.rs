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

pub(super) unsafe extern "C" fn audio_play_callback(
    data: *mut c_void,
    samples: *const c_void,
    count: c_uint,
    _pts: i64,
) {
    unsafe {
        let (rb_prod, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
            .as_mut()
            .unwrap();

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
        let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
            .as_mut()
            .unwrap();
        player.set_stream_paused(true);
    }
}

pub(super) unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
            .as_mut()
            .unwrap();
        player.set_stream_paused(false);
    }
}

pub(super) unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
    unsafe {
        let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
            .as_mut()
            .unwrap();
        if player.is_instance_valid() {
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
