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

use std::ptr::slice_from_raw_parts_mut;

use godot::{
    classes::{native::AudioFrame, AudioStreamPlayback, IAudioStreamPlayback},
    prelude::*,
};
use ringbuf::{traits::Consumer, HeapCons};

#[derive(GodotClass)]
#[class(base=AudioStreamPlayback, no_init, internal)]
pub struct InternalAudioStreamPlayback {
    base: Base<AudioStreamPlayback>,
    rb_cons: HeapCons<AudioFrame>,
}

impl InternalAudioStreamPlayback {
    pub fn create(rb_cons: HeapCons<AudioFrame>) -> Gd<Self> {
        Gd::from_init_fn(|base| Self { base, rb_cons })
    }

    pub fn clear_buffer(&mut self) {
        self.rb_cons.clear();
    }
}

#[godot_api]
impl IAudioStreamPlayback for InternalAudioStreamPlayback {
    unsafe fn mix_rawptr(&mut self, buffer: *mut AudioFrame, _rate_scale: f32, frames: i32) -> i32 {
        let buffer_slice = slice_from_raw_parts_mut(buffer, frames as usize)
            .as_mut()
            .unwrap();
        for (i, item) in buffer_slice.iter_mut().enumerate() {
            if let Some(frame) = self.rb_cons.try_pop() {
                *item = frame;
            } else {
                return i as i32;
            }
        }
        frames
    }

    fn start(&mut self, _from_pos: f64) {
        // do nothing
    }
    fn stop(&mut self) {
        // do nothing
    }
    fn is_playing(&self) -> bool {
        true
    }
}
