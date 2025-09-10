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

use godot::{
    classes::{native::AudioFrame, AudioStream, AudioStreamPlayback, IAudioStream},
    prelude::*,
};
use ringbuf::HeapCons;

use crate::vlc_media_player::internal_audio_stream_playback::InternalAudioStreamPlayback;

#[derive(GodotClass)]
#[class(base=AudioStream, internal, no_init)]
pub struct InternalAudioStream {
    base: Base<AudioStream>,
    pub playback: Gd<InternalAudioStreamPlayback>,
}

impl InternalAudioStream {
    pub fn create(rb_cons: HeapCons<AudioFrame>) -> Gd<Self> {
        let playback = InternalAudioStreamPlayback::create(rb_cons);
        Gd::from_init_fn(|base| Self { base, playback })
    }
}

#[godot_api]
impl IAudioStream for InternalAudioStream {
    fn instantiate_playback(&self) -> Option<Gd<AudioStreamPlayback>> {
        Some(self.playback.clone().upcast())
    }
}
