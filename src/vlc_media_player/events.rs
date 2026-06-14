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

use std::ffi::c_void;

use godot::prelude::*;

use crate::vlc::*;

use super::VlcMediaPlayer;
use super::software_video;

impl VlcMediaPlayer {
    pub(crate) fn register_player_callbacks(&mut self) {
        unsafe {
            let self_ptr = self.self_gd.as_mut().unwrap().as_mut() as *mut _;

            // The GPU output-callbacks API and the software callbacks API
            // are mutually exclusive at the libvlc level: register one or
            // the other, never both. On any GPU init failure we fall
            // through to the software path.
            let gpu_active = self.try_init_gpu_backend();
            if !gpu_active {
                libvlc_video_set_callbacks(
                    self.player_ptr,
                    Some(software_video::video_lock_callback),
                    Some(software_video::video_unlock_callback),
                    Some(software_video::video_display_callback),
                    self.video_tx.as_mut() as *mut _ as *mut c_void,
                );
                libvlc_video_set_format_callbacks(
                    self.player_ptr,
                    Some(software_video::video_format_callback),
                    Some(software_video::video_cleanup_callback),
                );
            }
            libvlc_audio_set_callbacks(
                self.player_ptr,
                Some(super::audio_callbacks::audio_play_callback),
                Some(super::audio_callbacks::audio_pause_callback),
                Some(super::audio_callbacks::audio_resume_callback),
                Some(super::audio_callbacks::audio_flush_callback),
                Some(super::audio_callbacks::audio_drain_callback),
                self.audio_prod.as_mut() as *mut _ as *mut c_void,
            );
            libvlc_audio_set_format_callbacks(
                self.player_ptr,
                Some(super::audio_callbacks::audio_setup_callback),
                Some(super::audio_callbacks::audio_cleanup_callback),
            );

            fn get_player(ptr: *mut c_void) -> Gd<VlcMediaPlayer> {
                unsafe { (ptr as *mut Gd<VlcMediaPlayer>).as_mut().unwrap().clone() }
            }
            let event_manager = libvlc_media_player_event_manager(self.player_ptr);

            unsafe extern "C" fn opening_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("openning").to_variant()]);
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("opening").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerOpening as libvlc_event_type_t,
                Some(opening_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn buffering_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("buffering").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerBuffering as libvlc_event_type_t,
                Some(buffering_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn playing_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("playing").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPlaying as libvlc_event_type_t,
                Some(playing_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn paused_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("paused").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPaused as libvlc_event_type_t,
                Some(paused_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn stopped_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("stopped").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerStopped as libvlc_event_type_t,
                Some(stopped_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn forward_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("forward").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerForward as libvlc_event_type_t,
                Some(forward_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn backward_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("backward").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerBackward as libvlc_event_type_t,
                Some(backward_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn stopping_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from("stopping").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerStopping as libvlc_event_type_t,
                Some(stopping_callback),
                self_ptr as *mut c_void,
            );
        }
    }
}
