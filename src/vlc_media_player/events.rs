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

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use godot::prelude::*;

use crate::vlc::*;

use super::VlcMediaPlayer;
use super::software_video;

/// How many events may wait for the main thread at once.
///
/// The queue is drained every frame, and the events that arrive most often --
/// the position and the time -- come about four times a second, so a queue this
/// deep means something is wrong rather than that playback is busy. The bound is
/// here so that a pathological producer cannot grow it without limit.
const PARKED_EVENT_CAPACITY: usize = 64;

/// One player event, on its way from libvlc's thread to the main one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParkedEvent {
    Opening,
    Playing,
    Paused,
    Stopped,
    Forward,
    Backward,
    Stopping,
    Position(f64),
    Time(i64),
    Length(i64),
    Seekable(bool),
    Pausable(bool),
}

/// Where the player's event callbacks leave what they received.
///
/// libvlc calls them back on its own threads -- the input thread, holding the
/// player's lock -- and from there neither Godot nor any `libvlc_media_player_*`
/// function may be touched (the player lock is not recursive). A callback
/// therefore only records the event; [VlcMediaPlayer::emit_parked_events] turns
/// whatever has piled up into signals on the main thread, once per frame.
///
/// Arrival order is kept, because it is part of what these events mean:
/// `stopping` comes before `stopped`, and the position of a reading before the
/// time of that same reading.
pub(crate) struct EventPark {
    queue: Mutex<VecDeque<ParkedEvent>>,
}

impl EventPark {
    pub(crate) fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(PARKED_EVENT_CAPACITY)),
        }
    }

    /// Records one event. Called from libvlc's threads.
    fn push(&self, event: ParkedEvent) {
        // A poisoned lock means another thread panicked while holding it. The
        // queue is still usable, and losing one event is a better outcome than
        // propagating a panic into libvlc's thread.
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() < PARKED_EVENT_CAPACITY {
            queue.push_back(event);
        }
    }

    /// Takes the oldest event, if there is one. Called from the main thread.
    fn pop(&self) -> Option<ParkedEvent> {
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.pop_front()
    }
}

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

// The readers for the events whose signal carries a value. Every payload is a
// member of the same union and the members overlap, so a reader aimed at the
// wrong member returns a plausible value rather than failing to compile -- which
// is what the round-trip tests at the bottom of this file exist to catch. Each
// of these takes an event of the type named after it.
unsafe fn read_position(event: *const libvlc_event_t) -> f64 {
    unsafe { (*event).u.media_player_position_changed.new_position }
}

unsafe fn read_time(event: *const libvlc_event_t) -> i64 {
    unsafe { (*event).u.media_player_time_changed.new_time }
}

unsafe fn read_length(event: *const libvlc_event_t) -> i64 {
    unsafe { (*event).u.media_player_length_changed.new_length }
}

unsafe fn read_seekable(event: *const libvlc_event_t) -> bool {
    unsafe { (*event).u.media_player_seekable_changed.new_seekable != 0 }
}

unsafe fn read_pausable(event: *const libvlc_event_t) -> bool {
    unsafe { (*event).u.media_player_pausable_changed.new_pausable != 0 }
}

/// Records an event from inside a libvlc callback.
///
/// # Safety
/// `user_data` must be the `EventPark` the callback was attached with, and that
/// park must still be alive.
unsafe fn park(user_data: *mut c_void, event: ParkedEvent) {
    unsafe { (*(user_data as *const EventPark)).push(event) }
}

impl VlcMediaPlayer {
    pub(crate) fn register_player_callbacks(&mut self) {
        unsafe {
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

            let event_manager = libvlc_media_player_event_manager(self.player_ptr);

            // Every callback below is attached with the park as its `user_data`,
            // so none of them can reach a Godot object: what they record is
            // emitted later, from the main thread. The buffering event is the
            // exception, and keeps its own slot, because it is the one event
            // whose repeated reports have to be compared rather than queued.
            let park_ptr = self.event_park.as_ref() as *const EventPark as *mut c_void;

            unsafe extern "C" fn opening_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Opening) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerOpening as libvlc_event_type_t,
                Some(opening_callback),
                park_ptr,
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
                // (`VlcMediaPlayer::on_notification`), and only when the value
                // moved. That is also why the atomic, and not the player object,
                // is what this event is attached with: nothing on the Godot side
                // is touched here.
                unsafe {
                    let percent = buffering_percent(event);
                    let slot = user_data as *const AtomicU32;
                    (*slot).store(percent.to_bits(), Ordering::Relaxed);
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
                unsafe { park(user_data, ParkedEvent::Playing) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPlaying as libvlc_event_type_t,
                Some(playing_callback),
                park_ptr,
            );

            unsafe extern "C" fn paused_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Paused) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPaused as libvlc_event_type_t,
                Some(paused_callback),
                park_ptr,
            );

            unsafe extern "C" fn stopped_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Stopped) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerStopped as libvlc_event_type_t,
                Some(stopped_callback),
                park_ptr,
            );

            unsafe extern "C" fn forward_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Forward) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerForward as libvlc_event_type_t,
                Some(forward_callback),
                park_ptr,
            );

            unsafe extern "C" fn backward_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Backward) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerBackward as libvlc_event_type_t,
                Some(backward_callback),
                park_ptr,
            );

            unsafe extern "C" fn stopping_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Stopping) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerStopping as libvlc_event_type_t,
                Some(stopping_callback),
                park_ptr,
            );

            unsafe extern "C" fn position_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Position(read_position(event))) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPositionChanged as libvlc_event_type_t,
                Some(position_callback),
                park_ptr,
            );

            unsafe extern "C" fn time_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Time(read_time(event))) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerTimeChanged as libvlc_event_type_t,
                Some(time_callback),
                park_ptr,
            );

            unsafe extern "C" fn length_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Length(read_length(event))) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerLengthChanged as libvlc_event_type_t,
                Some(length_callback),
                park_ptr,
            );

            unsafe extern "C" fn seekable_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Seekable(read_seekable(event))) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerSeekableChanged as libvlc_event_type_t,
                Some(seekable_callback),
                park_ptr,
            );

            unsafe extern "C" fn pausable_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Pausable(read_pausable(event))) };
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPausableChanged as libvlc_event_type_t,
                Some(pausable_callback),
                park_ptr,
            );
        }
    }

    /// Emits the signals for everything the callbacks have recorded since the
    /// last frame.
    ///
    /// Called from `on_notification`, on the main thread: one frame's worth of
    /// events goes out in the order they arrived.
    pub(crate) fn emit_parked_events(&mut self) {
        while let Some(event) = self.event_park.pop() {
            match event {
                ParkedEvent::Opening => self.signals().opening().emit(),
                ParkedEvent::Playing => self.signals().playing().emit(),
                ParkedEvent::Paused => self.signals().paused().emit(),
                ParkedEvent::Stopped => self.signals().stopped().emit(),
                ParkedEvent::Forward => self.signals().forward().emit(),
                ParkedEvent::Backward => self.signals().backward().emit(),
                ParkedEvent::Stopping => self.signals().stopping().emit(),
                ParkedEvent::Position(position) => {
                    self.signals().position_changed().emit(position);
                }
                ParkedEvent::Time(time) => {
                    self.signals().time_changed().emit(time);
                }
                ParkedEvent::Length(length) => {
                    self.signals().length_changed().emit(length);
                }
                ParkedEvent::Seekable(seekable) => {
                    self.signals().seekable_changed().emit(seekable);
                }
                ParkedEvent::Pausable(pausable) => {
                    self.signals().pausable_changed().emit(pausable);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a payload in the event libvlc would have sent.
    ///
    /// The payload is a union member, so a reader that picks the wrong member
    /// gets a plausible-looking value rather than an error: nothing at the type
    /// level ties a field to an event type. What a round trip pins down is that
    /// each reader reads the member libvlc writes; it stops passing the moment
    /// that accessor is pointed at another member.
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

    fn position_event(position: f64) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerPositionChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_position_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_11 {
                    new_position: position,
                },
            },
        }
    }

    fn time_event(time: i64) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerTimeChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_time_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_12 {
                    new_time: time,
                },
            },
        }
    }

    fn length_event(length: i64) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerLengthChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_length_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_24 {
                    new_length: length,
                },
            },
        }
    }

    fn seekable_event(seekable: bool) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerSeekableChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_seekable_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_14 {
                    new_seekable: seekable as i32,
                },
            },
        }
    }

    fn pausable_event(pausable: bool) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerPausableChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_pausable_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_15 {
                    new_pausable: pausable as i32,
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

    #[test]
    fn reads_the_value_each_progress_event_carries() {
        let position = position_event(0.25);
        assert_eq!(unsafe { read_position(&position) }, 0.25);

        let time = time_event(1234);
        assert_eq!(unsafe { read_time(&time) }, 1234);

        let length = length_event(3723000);
        assert_eq!(unsafe { read_length(&length) }, 3723000);

        // The capabilities arrive as C `int`s, so the reader is what turns them
        // into the flag the signal carries.
        assert!(unsafe { read_seekable(&seekable_event(true)) });
        assert!(!unsafe { read_seekable(&seekable_event(false)) });
        assert!(unsafe { read_pausable(&pausable_event(true)) });
        assert!(!unsafe { read_pausable(&pausable_event(false)) });
    }

    /// Only the oldest event may be handed out, and in the order it arrived: the
    /// order of `stopping` and `stopped` is what tells a handler which of the
    /// two it is looking at.
    #[test]
    fn hands_events_out_in_arrival_order() {
        let park = EventPark::new();
        park.push(ParkedEvent::Opening);
        park.push(ParkedEvent::Playing);
        park.push(ParkedEvent::Time(1500));
        park.push(ParkedEvent::Stopping);
        park.push(ParkedEvent::Stopped);

        for expected in [
            ParkedEvent::Opening,
            ParkedEvent::Playing,
            ParkedEvent::Time(1500),
            ParkedEvent::Stopping,
            ParkedEvent::Stopped,
        ] {
            assert_eq!(park.pop(), Some(expected));
        }
        assert_eq!(park.pop(), None);
    }

    /// The cap is what keeps a producer that has gone wrong from growing the
    /// queue without limit; the events that fit are still delivered in order.
    #[test]
    fn drops_events_once_it_is_full() {
        let park = EventPark::new();
        for _ in 0..PARKED_EVENT_CAPACITY + 10 {
            park.push(ParkedEvent::Time(1));
        }

        for _ in 0..PARKED_EVENT_CAPACITY {
            assert_eq!(park.pop(), Some(ParkedEvent::Time(1)));
        }
        assert_eq!(park.pop(), None);
    }
}
