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
use crate::vlc_track::c_string;

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
///
/// The track events carry an id, and that id is copied out of libvlc's event
/// rather than pointed at: it belongs to the track, which can be gone before the
/// main thread ever sees the record. That is why this is not `Copy` -- every other
/// variant is a plain value and could be.
///
/// The title selection event carries a whole title for the same reason, and
/// copies it the same way: see [ParkedTitle].
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParkedEvent {
    Opening,
    Playing,
    Paused,
    Stopped,
    Forward,
    Backward,
    Stopping,
    Error,
    Position(f64),
    Time(i64),
    Length(i64),
    Seekable(bool),
    Pausable(bool),
    TrackAdded(i32, String),
    TrackRemoved(i32, String),
    TrackUpdated(i32, String),
    TrackSelected(i32, String),
    TrackUnselected(i32, String),
    ChapterChanged(i32),
    TitleListChanged,
    TitleSelectionChanged(i32, ParkedTitle),
}

/// One title, as the event that selected it described it.
///
/// These are the same three values -- and the same three dictionary keys -- as one
/// entry of [VlcMediaPlayer::get_full_title_descriptions], but held as Rust values
/// instead of libvlc's structure. The event points into the frame of the call it
/// arrives in and borrows the name inside it, so the fields are copied while the
/// callback that received it is still running, exactly as the track events copy
/// their ids.
///
/// The dictionary is built on the main thread, from this, rather than in the
/// callback: the signal carries a `Dictionary` and constructing one is a Godot
/// call, which a libvlc thread may not make.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParkedTitle {
    name: String,
    duration: i64,
    flags: i32,
}

impl ParkedTitle {
    fn dictionary(&self) -> VarDictionary {
        // The same three keys as a getter entry, built by the same function: the
        // signal is documented as carrying one of those, and a second copy here is
        // how the two would drift apart.
        super::title_dictionary(self.name.clone(), self.duration, self.flags)
    }
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

/// Reads the track type and id out of a `libvlc_MediaPlayerESAdded`,
/// `ESDeleted` or `ESUpdated` event.
///
/// All three carry the same payload. `psz_id` is libvlc's own string, owned by the
/// track it names, so it is copied here: the callback runs on libvlc's thread and
/// may not keep anything past it, and the track can be gone before the main thread
/// reads the record. `i_id` is deprecated in libvlc's own header and is not read.
///
/// # Safety
/// `event` must be one of those three event types.
#[allow(clippy::unnecessary_cast)]
unsafe fn read_es_changed(event: *const libvlc_event_t) -> (i32, String) {
    let payload = unsafe { (*event).u.media_player_es_changed };
    (payload.i_type as i32, c_string(payload.psz_id))
}

/// Reads a `libvlc_MediaPlayerESSelected` event, which is one of two things: a
/// track that joined the selection, or one that left it.
///
/// libvlc fills exactly one of the two ids, and that is what says which of the two
/// this is -- not `i_type`, which is the same for both directions. `i_type` is
/// only read once an id has said the event is one of the two, because libvlc
/// writes it inside those branches alone: an event with neither id set would
/// otherwise hand back uninitialised stack memory of libvlc's frame. That case is
/// reported as `None`, and nothing is emitted for it.
///
/// # Safety
/// `event` must be a `libvlc_MediaPlayerESSelected` event.
#[allow(clippy::unnecessary_cast)]
unsafe fn read_es_selection(event: *const libvlc_event_t) -> Option<ParkedEvent> {
    let payload = unsafe { (*event).u.media_player_es_selection_changed };
    if !payload.psz_selected_id.is_null() {
        let track_type = payload.i_type as i32;
        Some(ParkedEvent::TrackSelected(
            track_type,
            c_string(payload.psz_selected_id),
        ))
    } else if !payload.psz_unselected_id.is_null() {
        let track_type = payload.i_type as i32;
        Some(ParkedEvent::TrackUnselected(
            track_type,
            c_string(payload.psz_unselected_id),
        ))
    } else {
        None
    }
}

/// Reads the chapter number out of a `libvlc_MediaPlayerChapterChanged` event.
///
/// The number is all libvlc puts in it: the title it belongs to and the chapter's
/// name are both in libvlc's hands where it raises this, and neither is passed on.
///
/// # Safety
/// `event` must be a `libvlc_MediaPlayerChapterChanged` event.
#[allow(clippy::unnecessary_cast)]
unsafe fn read_chapter_changed(event: *const libvlc_event_t) -> i32 {
    unsafe { (*event).u.media_player_chapter_changed.new_chapter as i32 }
}

/// Reads the title out of a `libvlc_MediaPlayerTitleSelectionChanged` event.
///
/// libvlc fills both fields of the payload, but only for the length of the call
/// it is making: the pointer is to its own bookkeeping for the title and the name
/// inside is borrowed rather than owned, so everything wanted from it is copied
/// here, in the callback, and nothing is kept. `index` is libvlc's own count of
/// the title that was selected.
///
/// A payload naming no title is reported as `None` and nothing is emitted for it:
/// such an event says nothing this signal is for, and an empty dictionary would
/// say something it does not mean. The pinned runtime does not send one.
///
/// # Safety
/// `event` must be a `libvlc_MediaPlayerTitleSelectionChanged` event.
#[allow(clippy::unnecessary_cast)]
unsafe fn read_title_selection(event: *const libvlc_event_t) -> Option<ParkedEvent> {
    let payload = unsafe { (*event).u.media_player_title_selection_changed };
    if payload.title.is_null() {
        return None;
    }
    let title = unsafe { &*payload.title };
    Some(ParkedEvent::TitleSelectionChanged(
        payload.index as i32,
        ParkedTitle {
            name: c_string(title.psz_name),
            duration: title.i_duration,
            flags: title.i_flags as i32,
        },
    ))
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerStopping as libvlc_event_type_t,
                Some(stopping_callback),
                park_ptr,
            );

            // What libvlc raises when the input gives up: the media cannot be
            // opened, the demuxer failed, the stream output could not be started.
            // Nothing is read out of the event, and not because the payload is
            // uninteresting: this event has none. The union in `libvlc_events.h`
            // has no member for it and libvlc's sender fills in nothing but the
            // type, so reading `u` here would read whatever was on that stack.
            unsafe extern "C" fn error_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Error) };
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerEncounteredError as libvlc_event_type_t,
                Some(error_callback),
                park_ptr,
            );

            unsafe extern "C" fn position_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::Position(read_position(event))) };
            }
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
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
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerPausableChanged as libvlc_event_type_t,
                Some(pausable_callback),
                park_ptr,
            );

            // The four track events. libvlc raises them on the input thread while
            // it holds the player's lock, and it raises one per track that moved
            // rather than one per call: replacing a selection of three with one of
            // one arrives as three unselects and then a select. Arrival order is
            // therefore part of what they mean, and the queue keeps it.
            unsafe extern "C" fn es_added_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let (track_type, id) = unsafe { read_es_changed(event) };
                unsafe { park(user_data, ParkedEvent::TrackAdded(track_type, id)) };
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerESAdded as libvlc_event_type_t,
                Some(es_added_callback),
                park_ptr,
            );

            unsafe extern "C" fn es_deleted_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let (track_type, id) = unsafe { read_es_changed(event) };
                unsafe { park(user_data, ParkedEvent::TrackRemoved(track_type, id)) };
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerESDeleted as libvlc_event_type_t,
                Some(es_deleted_callback),
                park_ptr,
            );

            unsafe extern "C" fn es_updated_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let (track_type, id) = unsafe { read_es_changed(event) };
                unsafe { park(user_data, ParkedEvent::TrackUpdated(track_type, id)) };
            }
            // Its value is not next to the other three -- `ESAdded` is 276 and
            // `ESUpdated` is 285, with the cork, mute and volume events in
            // between -- so this is attached on its own name rather than by
            // counting from `ESAdded`.
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerESUpdated as libvlc_event_type_t,
                Some(es_updated_callback),
                park_ptr,
            );

            unsafe extern "C" fn es_selected_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                if let Some(event) = unsafe { read_es_selection(event) } {
                    unsafe { park(user_data, event) };
                }
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t,
                Some(es_selected_callback),
                park_ptr,
            );

            // The three chapter and title events. Each is attached on the name
            // the generated bindings gave it rather than on a value counted from
            // a neighbour: that enumeration is re-anchored by hand whenever libvlc
            // comments an entry out -- `TitleChanged` is commented out in the
            // pinned header, and the entries after it moved with it -- so anything
            // computed from a neighbour is exactly what goes wrong quietly.
            unsafe extern "C" fn chapter_changed_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let chapter = unsafe { read_chapter_changed(event) };
                unsafe { park(user_data, ParkedEvent::ChapterChanged(chapter)) };
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerChapterChanged as libvlc_event_type_t,
                Some(chapter_changed_callback),
                park_ptr,
            );

            // Its payload is empty, and libvlc leaves the union uninitialised
            // rather than zeroing it, so this callback reads nothing: the event
            // only says to ask for the titles again.
            unsafe extern "C" fn title_list_changed_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                unsafe { park(user_data, ParkedEvent::TitleListChanged) };
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerTitleListChanged as libvlc_event_type_t,
                Some(title_list_changed_callback),
                park_ptr,
            );

            unsafe extern "C" fn title_selection_changed_callback(
                event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                if let Some(event) = unsafe { read_title_selection(event) } {
                    unsafe { park(user_data, event) };
                }
            }
            self.attachments.attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerTitleSelectionChanged as libvlc_event_type_t,
                Some(title_selection_changed_callback),
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
                ParkedEvent::Error => self.signals().error().emit(),
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
                ParkedEvent::TrackAdded(track_type, id) => {
                    self.signals()
                        .track_added()
                        .emit(track_type, &GString::from(id.as_str()));
                }
                ParkedEvent::TrackRemoved(track_type, id) => {
                    self.signals()
                        .track_removed()
                        .emit(track_type, &GString::from(id.as_str()));
                }
                ParkedEvent::TrackUpdated(track_type, id) => {
                    self.signals()
                        .track_updated()
                        .emit(track_type, &GString::from(id.as_str()));
                }
                ParkedEvent::TrackSelected(track_type, id) => {
                    self.signals()
                        .track_selected()
                        .emit(track_type, &GString::from(id.as_str()));
                }
                ParkedEvent::TrackUnselected(track_type, id) => {
                    self.signals()
                        .track_unselected()
                        .emit(track_type, &GString::from(id.as_str()));
                }
                ParkedEvent::ChapterChanged(chapter) => {
                    self.signals().chapter_changed().emit(chapter);
                }
                ParkedEvent::TitleListChanged => self.signals().title_list_changed().emit(),
                ParkedEvent::TitleSelectionChanged(index, title) => {
                    self.signals()
                        .title_selection_changed()
                        .emit(index, &title.dictionary());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // The bindgen enum types differ per target -- `libvlc_track_type_t` is `c_int` on
    // Windows and `u32` on Linux and Android -- so the casts to `i32` below are
    // required by some targets and redundant on the others, exactly as in
    // `vlc_media_player.rs` and `acceptance.rs`.
    #![allow(clippy::unnecessary_cast)]

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

    /// The payload `ESAdded`, `ESDeleted` and `ESUpdated` share.
    fn es_changed_event(
        event_type: libvlc_event_e,
        track_type: libvlc_track_type_t,
        id: *const std::ffi::c_char,
    ) -> libvlc_event_t {
        libvlc_event_t {
            type_: event_type as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_es_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_27 {
                    i_type: track_type,
                    i_id: 0,
                    psz_id: id,
                },
            },
        }
    }

    /// The payload `ESSelected` uses, which is a different member: it names the
    /// track that left the selection and the one that joined it.
    fn es_selection_event(
        track_type: libvlc_track_type_t,
        unselected_id: *const std::ffi::c_char,
        selected_id: *const std::ffi::c_char,
    ) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerESSelected as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_es_selection_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_28 {
                    i_type: track_type,
                    psz_unselected_id: unselected_id,
                    psz_selected_id: selected_id,
                },
            },
        }
    }

    /// The payload `ChapterChanged` uses, which is a number and nothing else.
    fn chapter_changed_event(chapter: i32) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerChapterChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_chapter_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_10 {
                    new_chapter: chapter,
                },
            },
        }
    }

    /// The payload `TitleSelectionChanged` uses: the title that was selected and
    /// its index. Both are pointers into libvlc's own frame, which is why the
    /// reader under test has to copy rather than keep.
    fn title_selection_event(
        index: i32,
        title: *const libvlc_title_description_t,
    ) -> libvlc_event_t {
        libvlc_event_t {
            type_: libvlc_event_e_libvlc_MediaPlayerTitleSelectionChanged as libvlc_event_type_t,
            p_obj: std::ptr::null_mut(),
            u: libvlc_event_t__bindgen_ty_1 {
                media_player_title_selection_changed: libvlc_event_t__bindgen_ty_1__bindgen_ty_13 {
                    title,
                    index,
                },
            },
        }
    }

    /// A title description the way libvlc would fill one in.
    ///
    /// The name points at a `'static` literal, which stands in for libvlc's own
    /// storage: what the reader has to do with either is the same, which is copy
    /// the characters out and keep none of the pointer.
    fn title_description(
        name: *const std::ffi::c_char,
        duration: i64,
        flags: u32,
    ) -> libvlc_title_description_t {
        libvlc_title_description_t {
            i_duration: duration,
            psz_name: name.cast_mut(),
            i_flags: flags,
        }
    }

    /// The chapter and title events: one carries a number, the other a whole
    /// title that has to be copied out of the frame it arrives in.
    #[test]
    fn reads_the_chapter_and_title_events() {
        assert_eq!(
            unsafe { read_chapter_changed(&chapter_changed_event(0)) },
            0
        );
        assert_eq!(
            unsafe { read_chapter_changed(&chapter_changed_event(3)) },
            3
        );

        let title = title_description(c"Title 0 [00:00:04]".as_ptr(), 4000, 0);
        assert_eq!(
            unsafe { read_title_selection(&title_selection_event(0, &title)) },
            Some(ParkedEvent::TitleSelectionChanged(
                0,
                ParkedTitle {
                    name: String::from("Title 0 [00:00:04]"),
                    duration: 4000,
                    flags: 0,
                }
            ))
        );

        // A title whose length libvlc does not know arrives as `0`, and a menu
        // title's flags arrive as they are: the reader is not a filter.
        let menu = title_description(c"Main Menu".as_ptr(), 0, 1);
        assert_eq!(
            unsafe { read_title_selection(&title_selection_event(2, &menu)) },
            Some(ParkedEvent::TitleSelectionChanged(
                2,
                ParkedTitle {
                    name: String::from("Main Menu"),
                    duration: 0,
                    flags: 1,
                }
            ))
        );

        // A payload that names no title is not a title with empty fields, and
        // nothing is emitted for it: there would be no name to report while the
        // index would claim there was one.
        assert_eq!(
            unsafe { read_title_selection(&title_selection_event(0, std::ptr::null())) },
            None
        );
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

    /// The track events: three of them share one payload, and the fourth is a
    /// different member whose direction is decided by which id is set.
    #[test]
    fn reads_the_track_events() {
        let added = es_changed_event(
            libvlc_event_e_libvlc_MediaPlayerESAdded,
            libvlc_track_type_t_libvlc_track_video,
            c"video/1".as_ptr(),
        );
        assert_eq!(
            unsafe { read_es_changed(&added) },
            (
                libvlc_track_type_t_libvlc_track_video as i32,
                String::from("video/1")
            )
        );

        let selected = es_selection_event(
            libvlc_track_type_t_libvlc_track_text,
            std::ptr::null(),
            c"spu/0".as_ptr(),
        );
        assert_eq!(
            unsafe { read_es_selection(&selected) },
            Some(ParkedEvent::TrackSelected(
                libvlc_track_type_t_libvlc_track_text as i32,
                String::from("spu/0")
            ))
        );

        let unselected = es_selection_event(
            libvlc_track_type_t_libvlc_track_text,
            c"spu/0".as_ptr(),
            std::ptr::null(),
        );
        assert_eq!(
            unsafe { read_es_selection(&unselected) },
            Some(ParkedEvent::TrackUnselected(
                libvlc_track_type_t_libvlc_track_text as i32,
                String::from("spu/0")
            ))
        );

        // Neither id set is not one of the two things libvlc sends -- and the type
        // beside them is only written when one of them is, so this is reported as
        // nothing rather than as a signal carrying an empty id.
        let neither = es_selection_event(
            libvlc_track_type_t_libvlc_track_text,
            std::ptr::null(),
            std::ptr::null(),
        );
        assert_eq!(unsafe { read_es_selection(&neither) }, None);
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
