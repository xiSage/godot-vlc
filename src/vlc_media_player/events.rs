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
use std::sync::atomic::{AtomicU32, Ordering};

use godot::prelude::*;

use crate::vlc::*;

use super::VlcMediaPlayer;
use super::software_video;

/// Reads the percentage out of a `libvlc_MediaPlayerBuffering` event.
///
/// libvlc computes the payload itself, as `100 * new_buffering`
/// (`lib/media_player.c`, `on_buffering_changed`), so what arrives is already
/// the 0-100 percentage a UI wants -- and it is exactly `100.0` when the buffer
/// is full, because the filled branch sends the literal `1.0`.
///
/// # Safety
/// `event` must point at a valid `libvlc_event_t` whose type is
/// `libvlc_MediaPlayerBuffering`.
unsafe fn buffering_percent(event: *const libvlc_event_t) -> f32 {
    unsafe { (*event).u.media_player_buffering.new_cache }
}

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
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                // Deliberately does not emit here. libvlc sends this event once
                // per PCR -- hundreds of times while one buffer fills -- and
                // `libvlc_event_send` makes the call on the input's own thread.
                // The value is parked in an atomic that the main thread reads,
                // and the signal is emitted from there, at most once per frame
                // (`VlcMediaPlayer::on_notification`). That is also why the
                // atomic, and not the player object, is what this event is
                // attached with: nothing on the Godot side is touched here.
                unsafe {
                    let percent = buffering_percent(event);
                    let park = user_data as *const AtomicU32;
                    (*park).store(percent.to_bits(), Ordering::Relaxed);
                }
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerBuffering as libvlc_event_type_t,
                Some(buffering_callback),
                self.buffering_percent.as_ref() as *const AtomicU32 as *mut c_void,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the event libvlc sends while a buffer fills.
    ///
    /// The payload is a union member, so a reader that picks the wrong member
    /// gets a plausible-looking number rather than an error: nothing at the type
    /// level ties `new_cache` to the buffering event. What a round trip pins
    /// down is that `buffering_percent` reads the member libvlc writes; it stops
    /// passing the moment that accessor is pointed at another one.
    fn buffering_event(percent: f32) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerBuffering as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_buffering: libvlc_event_t__bindgen_ty_1__bindgen_ty_9 {
                    new_cache: percent,
                },
            },
        }
    }

    /// The ends of the range are the ones a handler branches on: `0.0` is what
    /// libvlc sends when the input opens or a seek resets the buffer, and
    /// `100.0` is the exact value it sends once the buffer is full.
    #[test]
    fn reads_the_percentage_the_buffering_event_carries() {
        for percent in [0.0, 0.5, 42.5, 99.9, 100.0] {
            let event = buffering_event(percent);
            assert_eq!(unsafe { buffering_percent(&event) }, percent);
        }
    }
}
