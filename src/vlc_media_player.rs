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

mod audio_callbacks;
mod events;
mod internal_audio_stream;
pub mod internal_audio_stream_playback;
mod list_player;
mod software_video;
mod time_watch;

#[cfg(all(feature = "gpu", windows))]
mod gpu_d3d11;

use std::{
    ffi::c_int,
    ptr,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
};

use crate::{
    util::cstring_from_gstring,
    vlc::*,
    vlc_event_attachments::EventAttachments,
    vlc_instance::{self},
    vlc_media::VlcMedia,
    vlc_media_player::internal_audio_stream::InternalAudioStream,
    vlc_subtitle::VlcSubtitle,
    vlc_track::{VlcTrack, c_string},
    vlc_track_list::VlcTrackList,
};
use godot::{
    classes::{
        AudioServer, AudioStream, AudioStreamPlayer, Control, IControl, Image, ImageTexture,
        Texture2D, TextureRect,
        control::{LayoutPreset, LayoutPresetMode},
        image,
        native::AudioFrame,
        node::InternalMode,
        notify::ControlNotification,
        texture_rect::{ExpandMode, StretchMode as TextureRectStretchMode},
    },
    obj::NewAlloc,
    prelude::*,
};
use ringbuf::{HeapProd, HeapRb, traits::Split};

#[cfg(all(feature = "gpu", windows))]
use godot::classes::RenderingServer;

#[derive(GodotConvert, Var, Export, Clone, Debug)]
#[godot(via=i64)]
pub enum StretchMode {
    Scale,
    Tile,
    Keep,
    KeepCenterd,
    KeepAspect,
    KeepAspectCenterd,
    KeepAspectCovered,
}

#[derive(GodotConvert, Var, Export, Clone, Debug)]
#[godot(via=i64)]
pub enum MixTarget {
    Stereo,
    Surround,
    Center,
}

/// "libvlc has not reported a buffering percentage yet".
///
/// libvlc's own values are always inside `0..=100` (it computes them as
/// `100 * level`), so a negative sentinel cannot collide with a real report --
/// which matters, because the first report of a playthrough is a genuine `0.0`
/// and would otherwise be swallowed as "unchanged".
const NO_BUFFERING_REPORT: f32 = -1.0;

/// What libvlc says about the A to B loop, made safe to read.
///
/// `libvlc_media_player_get_abloop` writes its out-parameters before it knows
/// whether they mean anything, and in the "no loop" case two of them are
/// uninitialised stack memory rather than a value. Which of them is meaningful is
/// decided by the status, so that is the only thing this carries unguarded; the
/// rest are `-1`/`-1.0` where the status says they do not apply.
struct AbLoop {
    status: i32,
    a_time: i64,
    a_pos: f64,
    b_time: i64,
    b_pos: f64,
}

/// An array of descriptions libvlc allocated for the caller, and the count that
/// goes with it.
///
/// Both `get_full_*_descriptions` calls below hand out an array the caller has to
/// free with the matching release function, and both report how many entries it
/// holds -- as an `int`, while that release takes an `unsigned`. Pairing each
/// struct with its own release through [Description] is what keeps a chapter array
/// from being freed with the title release: the two layouts differ, and libvlc
/// would free pointers it read out of the middle of a struct.
///
/// The array is freed when this drops, so no early return can leak it. libvlc's
/// `0` is a real answer -- the chapter getter allocates an empty array for it,
/// which may come back NULL -- so a zero-length array is released as well, and
/// only a NULL one is skipped.
struct Descriptions<T: Description> {
    entries: *mut *mut T,
    count: u32,
}

impl<T: Description> Descriptions<T> {
    /// Takes over an array a getter returned.
    ///
    /// # Safety
    /// `entries` must be the array that `count` came back with, from the getter
    /// whose [Description] this is, and nothing else may free it afterwards.
    /// `count` is unsigned here on purpose: libvlc answers `-1` for "no array at
    /// all" and writes nothing, so the sign is what tells the two apart, and
    /// [dictionaries] is where that is decided -- once, before this is built.
    unsafe fn new(entries: *mut *mut T, count: u32) -> Self {
        Self { entries, count }
    }

    fn len(&self) -> usize {
        self.count as usize
    }

    /// # Safety
    /// `index` must be below [Self::len].
    unsafe fn get(&self, index: usize) -> &T {
        unsafe { &**self.entries.add(index) }
    }
}

impl<T: Description> Drop for Descriptions<T> {
    fn drop(&mut self) {
        if !self.entries.is_null() {
            unsafe { T::release(self.entries, self.count) }
        }
    }
}

/// One of the two description structs, together with the C function that frees an
/// array of it and the shape a caller sees it as.
///
/// The release function travels with the type it frees here rather than being
/// called at each site, which is also what lets [Descriptions] be generic over the
/// two.
trait Description: Sized {
    /// # Safety
    /// `entries` must be an array from the matching getter, holding `count`
    /// entries, and nothing else may free it afterwards.
    unsafe fn release(entries: *mut *mut Self, count: u32);

    /// The fields of one entry, as a dictionary. Every key is always present:
    /// there is no member of either struct that libvlc leaves unwritten.
    fn dictionary(&self) -> VarDictionary;
}

impl Description for libvlc_chapter_description_t {
    unsafe fn release(entries: *mut *mut Self, count: u32) {
        unsafe { libvlc_chapter_descriptions_release(entries, count) }
    }

    fn dictionary(&self) -> VarDictionary {
        let mut dict = VarDictionary::new();
        dict.set("name", c_string(self.psz_name));
        dict.set("time_offset", self.i_time_offset);
        dict.set("duration", self.i_duration);
        dict
    }
}

impl Description for libvlc_title_description_t {
    unsafe fn release(entries: *mut *mut Self, count: u32) {
        unsafe { libvlc_title_descriptions_release(entries, count) }
    }

    fn dictionary(&self) -> VarDictionary {
        title_dictionary(
            c_string(self.psz_name),
            self.i_duration,
            self.i_flags as i32,
        )
    }
}

/// The three fields of a title, as the dictionary both
/// [VlcMediaPlayer::get_full_title_descriptions] and [signal
/// VlcMediaPlayer::title_selection_changed] hand out.
///
/// One function rather than a copy at each site: the two are documented as
/// carrying exactly the same keys, and a second copy is how they stop matching.
fn title_dictionary(name: String, duration: i64, flags: i32) -> VarDictionary {
    let mut dict = VarDictionary::new();
    dict.set("name", name);
    dict.set("duration", duration);
    dict.set("flags", flags);
    dict
}

/// Turns what one of the `get_full_*_descriptions` calls below answered into the
/// array a script sees, freeing libvlc's array on the way out.
///
/// # Safety
/// `entries` and `count` must be exactly what the matching getter answered, and
/// nothing else may free `entries`.
unsafe fn dictionaries<T: Description>(entries: *mut *mut T, count: c_int) -> Array<VarDictionary> {
    if count < 0 {
        // libvlc's "no array at all": it wrote nothing to `entries`, so there is
        // neither a list to report nor anything to free. `0` is a different
        // answer and not this one -- it means libvlc allocated an empty array.
        return Array::new();
    }
    // The array belongs to `descriptions` from here, and it frees it however this
    // returns -- including on a panic in the loop below.
    let descriptions = unsafe { Descriptions::<T>::new(entries, count as u32) };
    let mut entries = Array::new();
    for index in 0..descriptions.len() {
        entries.push(&unsafe { descriptions.get(index) }.dictionary());
    }
    entries
}

/// A control used for video playback.\
/// This control provides a simple way to play video files using the VLC library. It supports most common video formats, including MP4, MKV, AVI, etc.
#[derive(GodotClass)]
#[class(base=Control, rename=VLCMediaPlayer)]
struct VlcMediaPlayer {
    base: Base<Control>,
    /// The media this player plays. Assigning one clears whatever it was
    /// showing first: see [signal frame_cleared]. Assigning `null` is not that
    /// -- libvlc keeps the media it already has, and the picture stays up.
    #[export]
    #[var(set=set_media)]
    media: Option<Gd<VlcMedia>>,
    #[export]
    autoplay: bool,
    /// Opt into the GPU output backend (libvlc renders into a D3D11 shared
    /// texture, our private D3D12 queue copies it into a Godot RD texture
    /// each frame). Requires Windows + `--rendering-driver d3d12`; on any
    /// prerequisite failure the player logs an error and falls back to the
    /// software path. Default `false` keeps the CPU pipeline.
    #[export]
    force_hardware: bool,
    #[export]
    #[var(set=set_stretch_mode)]
    stretch_mode: StretchMode,
    #[export(range = (-80.0, 24.0, suffix="db"))]
    #[var(set=set_volume_db)]
    volume_db: f32,
    #[export]
    #[var(set=set_mix_target)]
    mix_target: MixTarget,
    #[export]
    #[var(set=set_bus)]
    bus: StringName,
    player_ptr: *mut libvlc_media_player_t,
    /// Where the player's event callbacks leave what they received, for the main
    /// thread to turn into signals once a frame (`events.rs`). Boxed because the
    /// events are attached with this address, and it is written from libvlc's
    /// threads.
    event_park: Box<events::EventPark>,
    /// The buffering percentage libvlc reported last, as `f32::to_bits`, or
    /// [NO_BUFFERING_REPORT] while it has reported nothing. Boxed because this
    /// exact address is what the buffering event is attached with
    /// (`events.rs::register_player_callbacks`), and it is written from
    /// libvlc's input thread -- hence an atomic rather than an `f32`.
    buffering_percent: Box<AtomicU32>,
    /// The value last emitted on [signal buffering]. Main thread only: the
    /// signal is emitted from `on_notification`, never from the callback.
    buffering_reported: f32,
    /// The playback-time watcher (`time_watch.rs`): the latest point libvlc has
    /// reported, and the queue of watcher events for the main thread. Boxed because
    /// libvlc is handed this address once and it has to stay put for as long as the
    /// watcher is registered.
    time_watch: Box<time_watch::WatchSink>,
    /// The player's event callbacks, kept so that `Drop` can detach them
    /// (`vlc_event_attachments.rs`).
    ///
    /// It is not enough to release the player: libvlc stops it, joins its
    /// threads and destroys its event manager only when the reference count
    /// reaches zero, and a media list player retains the player it is given.
    /// This wrapper can therefore be freed while libvlc still holds the player,
    /// and then the callbacks would go on running with the user data that was
    /// freed along with this object.
    attachments: EventAttachments,
    texture: Gd<ImageTexture>,
    texture_rect: Gd<TextureRect>,
    /// The most recent frame the software output produced, kept so that
    /// [method get_frame] can hand it back. `None` until one arrives, and `None`
    /// forever when the GPU output is the one running -- that path keeps its frames
    /// in a texture this binding does not read back.
    frame: Option<Gd<Image>>,
    /// True between clearing the picture and the first frame of the video
    /// output that follows it.
    ///
    /// Changing the media sends the frames of the output that is going away
    /// down the same channel as the new output's, and the last frame libvlc
    /// produced usually sits in that channel when the change happens. Those
    /// frames carry no mark of their own, so this window is what tells them
    /// apart: while it is open, a frame that is not the first one of a video
    /// output is dropped instead of being shown.
    awaiting_output: bool,
    video_tx: Box<mpsc::Sender<(bool, Gd<Image>)>>, // (is_resized, image)
    video_rx: mpsc::Receiver<(bool, Gd<Image>)>,
    audio_prod: Box<(HeapProd<AudioFrame>, Gd<AudioStreamPlayer>)>,
    audio_player: Gd<AudioStreamPlayer>,
    /// libvlc-side `Arc<Backend>` ref; the libvlc-side ref is held via the
    /// opaque pointer passed to `libvlc_video_set_output_callbacks`. Both
    /// drop when the player is destroyed (libvlc's via `cleanup_cb`).
    #[cfg(all(feature = "gpu", windows))]
    gpu_backend: Option<std::sync::Arc<gpu_d3d11::output_callbacks::Backend>>,
    #[cfg(all(feature = "gpu", windows))]
    gpu_mailbox: Option<std::sync::Arc<gpu_d3d11::event_queue::EventMailbox>>,
    /// Per-frame importer captured by the `frame_pre_draw` Callable below;
    /// disconnecting that Callable on Drop releases the ref.
    #[cfg(all(feature = "gpu", windows))]
    gpu_importer: Option<std::sync::Arc<gpu_d3d11::importer::ImporterTask>>,
    /// True while the GPU path shows nothing because a media change cleared the
    /// picture: `texture_rect` is pointed back at the software texture, which
    /// is the same blank [member texture] the software path clears with.
    #[cfg(all(feature = "gpu", windows))]
    gpu_picture_hidden: bool,
    /// The destination texture the GPU path was showing when the picture was
    /// cleared. The picture goes back up once the importer builds another one:
    /// a texture this player did not have before belongs to the video output of
    /// the media that replaced the old one.
    #[cfg(all(feature = "gpu", windows))]
    gpu_hidden_rid: Option<Rid>,
    #[cfg(all(feature = "gpu", windows))]
    gpu_frame_callable: Option<Callable>,
}

#[godot_api]
impl IControl for VlcMediaPlayer {
    fn init(base: Base<Control>) -> Self {
        let player_ptr = unsafe {
            let instance = vlc_instance::get();
            libvlc_media_player_new(instance)
        };
        let texture = ImageTexture::new_gd();
        let mut texture_rect = TextureRect::new_alloc();
        texture_rect.set_texture(&texture);

        let (video_tx, video_rx) = mpsc::channel();
        let video_tx = Box::new(video_tx);
        let mut audio_player = AudioStreamPlayer::new_alloc();
        let audio_rb = HeapRb::new(AudioServer::singleton().get_mix_rate() as usize * 5);
        let (audio_rb_prod, audio_rb_cons) = audio_rb.split();
        let audio_prod = Box::new((audio_rb_prod, audio_player.clone()));
        let audio_stream = InternalAudioStream::create(audio_rb_cons);
        audio_player.set_stream(&audio_stream.upcast::<AudioStream>());
        Self {
            base,
            media: None,
            autoplay: false,
            force_hardware: false,
            stretch_mode: StretchMode::KeepAspectCenterd,
            volume_db: 0.0,
            mix_target: MixTarget::Stereo,
            bus: StringName::from("Master"),
            player_ptr,
            event_park: Box::new(events::EventPark::new()),
            buffering_percent: Box::new(AtomicU32::new(NO_BUFFERING_REPORT.to_bits())),
            buffering_reported: NO_BUFFERING_REPORT,
            time_watch: Box::new(time_watch::WatchSink::new()),
            attachments: EventAttachments::default(),
            texture,
            texture_rect: texture_rect.clone(),
            frame: None,
            awaiting_output: false,
            video_tx,
            video_rx,
            audio_prod,
            audio_player,
            #[cfg(all(feature = "gpu", windows))]
            gpu_backend: None,
            #[cfg(all(feature = "gpu", windows))]
            gpu_mailbox: None,
            #[cfg(all(feature = "gpu", windows))]
            gpu_importer: None,
            #[cfg(all(feature = "gpu", windows))]
            gpu_picture_hidden: false,
            #[cfg(all(feature = "gpu", windows))]
            gpu_hidden_rid: None,
            #[cfg(all(feature = "gpu", windows))]
            gpu_frame_callable: None,
        }
    }

    fn on_notification(&mut self, what: ControlNotification) {
        if what == ControlNotification::INTERNAL_PROCESS {
            // A picture a media change cleared goes back up on the frame that
            // finds a video output the importer did not have before.
            #[cfg(all(feature = "gpu", windows))]
            self.restore_gpu_picture();
            if let Ok(data) = self.video_rx.try_recv()
                && data.1.is_instance_valid()
                && !data.1.is_empty()
                && data.1.get_data_size() > 0
            {
                // The first frame of a video output is the one that ends the
                // window a media change opened (`awaiting_output`); anything
                // else that arrives while it is open belongs to the output that
                // just went away.
                if data.0 {
                    self.awaiting_output = false;
                }
                if !self.awaiting_output {
                    if data.0 {
                        self.texture.set_image(&data.1);
                    } else {
                        self.texture.update(&data.1);
                    }
                    // Kept so that a caller can look at the frame itself: the texture
                    // cannot be read back on every rendering driver, and the software
                    // output path already has the image in hand here.
                    self.frame = Some(data.1.clone());
                    self.signals().video_frame().emit();
                }
            }
            // Everything libvlc has reported since the last frame goes out here,
            // on the main thread, in the order it arrived.
            self.emit_parked_events();
            // libvlc's log is drained by the same frame, for the same reason and
            // with the same limitation: `VLCInstance` is an `Object` and has no
            // frame of its own, so a player is what carries its signal. The console
            // line was already written where the message arrived.
            crate::vlc_instance::drain_parked_logs();
            // The time watcher's events go out from the same frame: a script that
            // asked to be told when the clock moved is told here, on the main thread,
            // with the point libvlc handed over on one of its own.
            self.emit_watched_time();
            // The buffering value is reported from here, not from the event
            // callback, so that a burst of upstream reports costs one signal
            // per frame at most -- and only when the value actually moved.
            // `Relaxed` is enough: this is a progress value, not a
            // synchronisation channel.
            let percent = f32::from_bits(self.buffering_percent.load(Ordering::Relaxed));
            if percent != self.buffering_reported {
                self.buffering_reported = percent;
                self.signals().buffering().emit(percent);
            }
        } else if what == ControlNotification::READY {
            self.register_player_callbacks();
            let texture_rect = self.texture_rect.clone();
            self.base_mut()
                .add_child_ex(&texture_rect)
                .internal(InternalMode::FRONT)
                .done();
            self.texture_rect
                .set_anchors_and_offsets_preset_ex(LayoutPreset::FULL_RECT)
                .resize_mode(LayoutPresetMode::KEEP_SIZE)
                .done();
            self.texture_rect.set_expand_mode(ExpandMode::IGNORE_SIZE);

            let audio_player = self.audio_player.clone();
            self.base_mut()
                .add_child_ex(&audio_player.clone())
                .internal(InternalMode::FRONT)
                .done();

            self.update_media();
            self.update_stretch_mode();
            self.update_volume_db();
            self.update_mix_target();
            self.update_bus();

            self.base_mut().set_process_internal(true);
            if self.autoplay {
                self.play();
            }
        }
    }
}

impl Drop for VlcMediaPlayer {
    fn drop(&mut self) {
        // A registered time watcher has to go before the player it is registered on:
        // libvlc keeps the timer inside the player, and its own teardown does not
        // remove it. The sink knows whether there is one -- asking libvlc to unwatch
        // when there is nothing to unwatch is a crash rather than an error.
        self.time_watch.unregister(self.player_ptr);
        // Disconnect the per-frame callable BEFORE releasing the player —
        // an in-flight frame_pre_draw must not run against a half-torn
        // ImporterTask.
        #[cfg(all(feature = "gpu", windows))]
        if let Some(c) = self.gpu_frame_callable.take() {
            RenderingServer::singleton().disconnect(&StringName::from("frame_pre_draw"), &c);
        }
        // Detach before releasing: the release stops the player and joins its
        // threads, but only when this is the last reference to it, and these
        // callbacks carry user data that lives in this object. Detaching is
        // what makes that independent of the reference count, and it also
        // happens before the release's own teardown, so nothing is delivered
        // into a player that is on its way out.
        unsafe {
            self.attachments
                .detach_all(libvlc_media_player_event_manager(self.player_ptr));
        }
        unsafe {
            libvlc_media_player_release(self.player_ptr);
        }
        if self.texture_rect.is_instance_valid() {
            self.texture_rect.queue_free();
        }
        if self.audio_player.is_instance_valid() {
            self.audio_player.queue_free();
        }
    }
}

#[allow(clippy::unnecessary_cast)]
#[godot_api]
impl VlcMediaPlayer {
    #[constant]
    const STATE_NOTHING_SPECIAL: i32 = libvlc_state_t_libvlc_NothingSpecial as i32;
    #[constant]
    const STATE_OPENING: i32 = libvlc_state_t_libvlc_Opening as i32;
    #[constant]
    const STATE_BUFFERING: i32 = libvlc_state_t_libvlc_Buffering as i32;
    #[constant]
    const STATE_PLAYING: i32 = libvlc_state_t_libvlc_Playing as i32;
    #[constant]
    const STATE_PAUSED: i32 = libvlc_state_t_libvlc_Paused as i32;
    #[constant]
    const STATE_STOPPED: i32 = libvlc_state_t_libvlc_Stopped as i32;
    #[constant]
    const STATE_STOPPING: i32 = libvlc_state_t_libvlc_Stopping as i32;
    #[constant]
    const STATE_ERROR: i32 = libvlc_state_t_libvlc_Error as i32;

    #[constant]
    const NAVIGATE_ACTIVATE: i32 = libvlc_navigate_mode_t_libvlc_navigate_activate as i32;
    #[constant]
    const NAVIGATE_UP: i32 = libvlc_navigate_mode_t_libvlc_navigate_up as i32;
    #[constant]
    const NAVIGATE_DOWN: i32 = libvlc_navigate_mode_t_libvlc_navigate_down as i32;
    #[constant]
    const NAVIGATE_LEFT: i32 = libvlc_navigate_mode_t_libvlc_navigate_left as i32;
    #[constant]
    const NAVIGATE_RIGHT: i32 = libvlc_navigate_mode_t_libvlc_navigate_right as i32;
    #[constant]
    const NAVIGATE_POPUP: i32 = libvlc_navigate_mode_t_libvlc_navigate_popup as i32;

    #[constant]
    const POSITION_DISABLE: c_int = libvlc_position_t_libvlc_position_disable;
    #[constant]
    const POSITION_CENTER: c_int = libvlc_position_t_libvlc_position_center;
    #[constant]
    const POSITION_LEFT: c_int = libvlc_position_t_libvlc_position_left;
    #[constant]
    const POSITION_RIGHT: c_int = libvlc_position_t_libvlc_position_right;
    #[constant]
    const POSITION_TOP: c_int = libvlc_position_t_libvlc_position_top;
    #[constant]
    const POSITION_TOP_LEFT: c_int = libvlc_position_t_libvlc_position_top_left;
    #[constant]
    const POSITION_TOP_RIGHT: c_int = libvlc_position_t_libvlc_position_top_right;
    #[constant]
    const POSITION_BOTTOM: c_int = libvlc_position_t_libvlc_position_bottom;
    #[constant]
    const POSITION_BOTTOM_LEFT: c_int = libvlc_position_t_libvlc_position_bottom_left;
    #[constant]
    const POSITION_BOTTOM_RIGHT: c_int = libvlc_position_t_libvlc_position_bottom_right;

    /// [method get_abloop_status] reports this when no loop is set.
    #[constant]
    const ABLOOP_NONE: i32 = libvlc_abloop_t_libvlc_abloop_none as i32;
    /// [method get_abloop_status] reports this when only the A point is set.
    /// libvlc can be in this state; nothing in this binding leaves it there.
    #[constant]
    const ABLOOP_A: i32 = libvlc_abloop_t_libvlc_abloop_a as i32;
    /// [method get_abloop_status] reports this when both points are set, which
    /// is the state a loop has to be in to run.
    #[constant]
    const ABLOOP_B: i32 = libvlc_abloop_t_libvlc_abloop_b as i32;

    /// The flag [method get_full_title_descriptions] reports on a title whose
    /// content is a menu, for [constant NAVIGATE_ACTIVATE] and its siblings.
    #[constant]
    const TITLE_MENU: i32 = libvlc_title_menu as i32;
    /// The same, for a title whose content is interactive.
    ///
    /// Both come from libvlc's `libvlc_title_description_t.i_flags`, which is `0`
    /// for a plain title; libvlc has no constant for that case and this binding
    /// adds none, so `flags == 0` is the test for it.
    #[constant]
    const TITLE_INTERACTIVE: i32 = libvlc_title_interactive as i32;

    #[signal]
    fn opening();
    /// Emitted while the input is filling its buffer, with the completion
    /// percentage libvlc reports: [param percent] is `0`-`100`, and libvlc sends
    /// the literal `1.0` once the buffer is full, so `100` is exact.
    ///
    /// Only a report that differs from the previous one is emitted, and at most
    /// once per frame: libvlc raises this event once per PCR -- hundreds of
    /// times while a single buffer fills -- and a handler cannot draw more often
    /// than that anyway. [method get_buffering_percent] reads the same value
    /// without listening for it.
    ///
    /// # Warning
    /// - `0` means the input is opening, or that a seek reset the buffer; `100`
    ///   means the buffer filled. There is no separate "started"/"finished"
    ///   event, only these values.
    /// - A buffer that is cut short -- [method stop_async], a decode error, a
    ///   media that ends -- never gets a closing `100`. Do not read "reached
    ///   100" as "playback started"; ask [method get_state] for
    ///   [constant STATE_PLAYING].
    /// - The order against [signal playing] and [signal stopped] is not defined,
    ///   and a late report can still arrive after [signal stopping].
    /// - It arrives only while the input is buffering, not periodically for the
    ///   whole of playback.
    /// - This signal carries an argument now. A handler written before it did --
    ///   one that takes no parameters -- is no longer called by Godot, which
    ///   reports the argument mismatch each time the signal is emitted.
    #[signal]
    fn buffering(percent: f32);
    #[signal]
    fn playing();
    #[signal]
    fn paused();
    #[signal]
    fn stopped();
    #[signal]
    fn forward();
    #[signal]
    fn backward();
    #[signal]
    fn stopping();
    /// Emitted when the input gives up on a media: a file that is not there, a
    /// URL that answers 404 or refuses the connection, a demuxer that fails, a
    /// stream output that cannot be started.
    ///
    /// It is the only report of a failure that happens after [method play]
    /// returns: that call answers `0` for a media it cannot open, and the error
    /// state ([constant STATE_ERROR]) is not one a caller can observe -- a failed
    /// open goes through the same [signal stopping] and [signal stopped] as a
    /// media that ended, so polling [method get_state] cannot tell the two apart.
    ///
    /// # Warning
    /// - It does not cover every failure. A decoder that cannot cope -- a corrupt
    ///   bitstream, a codec that will not set up -- is not reported here: the
    ///   pictures stop arriving and the media ends as if it had played out,
    ///   [signal stopping] (end of stream) and then [signal stopped]. Nothing but
    ///   the engine log tells that apart from a normal end.
    /// - Silence is not proof that playback was fine. Stopping the player while
    ///   it is still opening discards a failure that had already happened, and an
    ///   input that fails before it starts reports nothing at all.
    /// - It is not quick. A missing local file reported it after roughly 140 ms in
    ///   testing, and a URL that had to time out took roughly 1.4 s.
    /// - It carries no argument: the libvlc event has no payload and no message
    ///   travels with it. The reason is in the engine log, as
    ///   [code]LibVLC: [ERROR] ...[/code].
    /// - It is raised at most once per input, and it can arrive after
    ///   [signal stopping].
    #[signal]
    fn error();
    /// Emitted with the playback position as a fraction of the media, `0.0`-`1.0`,
    /// together with [signal time_changed] and just before it.
    ///
    /// # Note
    /// - It is a notification, not a clock. libvlc paces it with its statistics
    ///   interval: at most about four times a second, often fewer, nothing while
    ///   paused, and nothing while the input is buffering. For a smooth progress
    ///   bar, read [method get_position] between them instead -- unlike this
    ///   signal, it is interpolated against the system clock.
    /// - While the input is buffering the position is reported as `0.0`, and the
    ///   first moments of playback report nothing at all, because libvlc treats a
    ///   timestamp of `0` as "unknown".
    /// - `0.0` is clamped, `1.0` is not: a container whose timestamps overshoot
    ///   its duration can report slightly more than one, so clamp before drawing
    ///   with it.
    #[signal]
    fn position_changed(position: f64);
    /// Emitted with the current playback time in milliseconds, just after
    /// [signal position_changed].
    ///
    /// # Note
    /// - Its pace is libvlc's, not the caller's: the statistics interval
    ///   (`stats-min-report-interval`, 250 ms by default) is a floor rather than a
    ///   period, the rate it settles at depends on the input and the container,
    ///   and it stops entirely while the input is buffering. [method get_time] is
    ///   the one to read for a smooth display.
    /// - It is also not a clock for gameplay. That is [signal time_point], which
    ///   arrives with every displayed frame or written block of audio and carries
    ///   the system date it was made at, and [method interpolate_time_point], which
    ///   reads the latest of those against the current system clock.
    /// - Playback ending does not produce a final value -- the last one can be
    ///   around 250 ms short of the end -- so finish a progress bar from
    ///   [method get_length] rather than by waiting for this to reach it.
    #[signal]
    fn time_changed(time: i64);
    /// Emitted with a time point libvlc reported: the clock a game can align itself
    /// with. Not emitted until [method watch_time] is called.
    ///
    /// # Parameters
    /// - [param ts_us] the media time of this point, in **microseconds**, or `-1`
    ///   while libvlc has none (`>= 0` or `-1` is libvlc's own contract).
    /// - [param position] the position, `0.0`-`1.0`.
    /// - [param rate] the playback rate of the player at that moment.
    /// - [param length_us] the media length in microseconds, or `0` while unknown.
    /// - [param system_date_us] the system date of this point, in microseconds, on
    ///   [method VLCInstance.get_clock_us]'s clock. It can be in the past or in the
    ///   future, and `9223372036854775807` (`INT64_MAX`) means the clock was paused
    ///   when this point was made -- the first point of a playback is often one of
    ///   those.
    ///
    /// # Note
    /// - **Do not subtract these two values to get "now".** libvlc's own header says
    ///   of [param ts_us] and [param system_date_us] that they "should not be used
    ///   directly", and this is why: [method interpolate_time_point] is what reads
    ///   them against a current system date, and it handles the paused-clock case and
    ///   the clamping to the media's length. The two are exposed because a caller
    ///   that schedules its own work needs them, not because the difference is the
    ///   answer.
    /// - Its pace is the output's, not the caller's: libvlc reports one each time a
    ///   video frame is displayed or a block of audio is written, so the interval is
    ///   somewhere between 5 ms and 10 seconds depending on the source (measured on
    ///   this runtime: about twenty a second for a ten-frame-a-second media with the
    ///   dummy video output and no audio). A caller that draws more often than that
    ///   interpolates; one that draws less often can ignore the points in between.
    /// - At most one is emitted per frame, and a point that arrives while another is
    ///   still waiting replaces it: an older point has nothing left to say once a
    ///   newer one exists. [method interpolate_time_point] always reads the newest,
    ///   whether or not its signal has gone out yet.
    /// - While a seek is in progress libvlc reports no points at all, and it says so
    ///   in its header; [signal time_point_seek] and [signal time_point_seek_finished]
    ///   bracket that.
    /// - The units differ from the rest of this class: milliseconds everywhere else,
    ///   microseconds here.
    #[signal]
    fn time_point(ts_us: i64, position: f64, rate: f64, length_us: i64, system_date_us: i64);
    /// Emitted when the player is paused, or is stopping. Only while
    /// [method watch_time] is registered.
    ///
    /// # Parameters
    /// - [param system_date_us] the system date of the event, in microseconds, on
    ///   [method VLCInstance.get_clock_us]'s clock -- **or `0` when the player is
    ///   stopping rather than pausing.** libvlc sends this one callback for both and
    ///   its date is only meaningful for a pause; [method get_state] is what tells
    ///   the two apart.
    ///
    /// # Note
    /// The point from before this is now stale: interpolating it would keep advancing
    /// a clock that has stopped. libvlc's own advice is to stop interpolating until
    /// the next update.
    #[signal]
    fn time_point_paused(system_date_us: i64);
    /// Emitted when a seek is asked for, with the point it was asked for. Only while
    /// [method watch_time] is registered.
    ///
    /// # Parameters
    /// The same five as [signal time_point].
    ///
    /// # Note
    /// [signal time_point_seek_finished] follows when the seek is over; between the
    /// two there are no [signal time_point] updates, because libvlc cannot report a
    /// point while the position is moving.
    #[signal]
    fn time_point_seek(ts_us: i64, position: f64, rate: f64, length_us: i64, system_date_us: i64);
    /// Emitted when the seek [signal time_point_seek] announced is over. Only while
    /// [method watch_time] is registered.
    ///
    /// # Note
    /// Points can be reported again from here on. libvlc's own signal for this is the
    /// seek callback with no point, which is why this one carries nothing.
    #[signal]
    fn time_point_seek_finished();
    /// Emitted with the length of the media in milliseconds, once libvlc knows it.
    ///
    /// # Note
    /// - It cannot arrive before [signal playing]: libvlc does not know the
    ///   length before playback starts.
    /// - Nothing is emitted while the length is unknown (a live stream, or a
    ///   demuxer that reports no duration): silence here is not a length of `0`.
    /// - It can arrive again for a corrected duration or for the next media on
    ///   this player, and [method get_length] rounds the same value, so the two
    ///   can differ by a millisecond.
    #[signal]
    fn length_changed(length: i64);
    /// Emitted when the input starts or stops being seekable.
    ///
    /// # Note
    /// - Only a change is emitted, so an input that cannot seek reports nothing
    ///   at all: no signal does not mean `false`. [method is_seekable] answers the
    ///   question directly, but also answers `false` before a media opens and
    ///   after it stops.
    /// - An input that is stopping usually reports `false` first, so a seek bar
    ///   can disable itself without polling.
    #[signal]
    fn seekable_changed(seekable: bool);
    /// Emitted when the input starts or stops being pausable.
    ///
    /// # Note
    /// - Only a change is emitted: an input that cannot be paused reports nothing
    ///   at all. [method can_pause] answers the question directly, but also
    ///   answers `false` before a media opens and after it stops.
    #[signal]
    fn pausable_changed(pausable: bool);
    /// Emitted when the software output path has turned a decoded frame into
    /// this player's texture.
    ///
    /// # Note
    /// - It does not arrive when the GPU output is the one running
    ///   ([method is_gpu_output_active]): that path copies frames into a
    ///   render-device texture from the rendering thread and reports nothing
    ///   back here, so a caller that watches this signal sees nothing at all.
    ///   [signal frame_cleared] is emitted by both paths.
    /// - One frame of this control is one frame into the texture: frames libvlc
    ///   produced faster than the control updates are taken from the queue on
    ///   the frames that follow, so the picture lags rather than skipping.
    #[signal]
    fn video_frame();
    /// Emitted when this player drops the picture it was showing because a
    /// different media was assigned to [member media].
    ///
    /// # What it is for
    /// Without it, changing the media leaves the old video on screen until the
    /// new one produces its first frame -- the old output's last frame is
    /// already on its way when the change happens. The picture is dropped here
    /// instead, and the frames that were on their way are held back: the
    /// texture shows nothing from this signal until the new media's own video
    /// output produces a frame.
    ///
    /// # Note
    /// - Only a change is emitted: assigning a media before anything has been
    ///   shown, assigning the media this player already has, and assigning
    ///   `null` (which leaves the picture up, see [member media]) are all
    ///   silent.
    /// - What ends the blank is a frame of the new video output, not this
    ///   signal: a media with no video track, or one that fails to open, leaves
    ///   this player showing nothing.
    /// - The texture is blanked in place, so a caller holding the object
    ///   [method get_texture] handed out sees the picture go away as well. It
    ///   is 1 by 1 and transparent while the player is blank; the size of the
    ///   video is back once a frame arrives.
    /// - Nothing is emitted for the media a media list player moves to on its
    ///   own: that player changes its item without this property being
    ///   assigned.
    #[signal]
    fn frame_cleared();
    /// Emitted when a track joins the input: at the moment a media opens and its
    /// tracks are created, and again when something adds one -- a subtitle
    /// attached while playing, a stream that reveals another track.
    ///
    /// # Parameters
    /// - [param track_type] the type of the track that arrived:
    ///   [constant VLCTrack.TYPE_AUDIO], [constant VLCTrack.TYPE_VIDEO] or
    ///   [constant VLCTrack.TYPE_TEXT].
    /// - [param id] the track's [method VLCTrack.get_id], which is what
    ///   [method get_track_from_id] and [method select_tracks_by_ids] take.
    ///
    /// # Note
    /// - It says a track exists, not that it is playable yet: the language, the
    ///   name and the codec of that id are readable through
    ///   [method get_track_from_id] as soon as the track list has been rebuilt.
    /// - A media opening produces one of these per track, in arrival order, and
    ///   [signal track_selected] follows for the ones libvlc selected on its own.
    /// - It carries the id rather than the track: a [VLCTrack] taken from
    ///   [method get_tracklist] is a snapshot, and the point of this signal is
    ///   that the one it was made from is out of date.
    /// - This signal carries arguments. A handler written for a signal that
    ///   carried none -- one that takes no parameters -- is no longer called by
    ///   Godot, which reports the argument mismatch each time it is emitted.
    #[signal]
    fn track_added(track_type: i32, id: GString);
    /// Emitted when a track leaves the input: a subtitle that failed to load, an
    /// input being torn down at the end of a media.
    ///
    /// # Parameters
    /// - [param track_type] the type of the track that left.
    /// - [param id] the id it had.
    ///
    /// # Note
    /// - The id is all that is left of it: [method get_track_from_id] will not
    ///   find it any more, by the time this arrives.
    /// - Tearing an input down -- the end of a media, [method stop_async] -- can
    ///   remove every track at once, and each one is reported.
    /// - This signal carries arguments; see the note in [signal track_added].
    #[signal]
    fn track_removed(track_type: i32, id: GString);
    /// Emitted when something about an existing track changed: a decoder
    /// reporting a different format for it, a track gaining a language or a name.
    ///
    /// # Parameters
    /// - [param track_type] the type of the track that changed.
    /// - [param id] the id it kept.
    ///
    /// # Note
    /// - The signal says *that* it changed, not what changed: ask
    ///   [method get_track_from_id] for the id, and compare what it reports with
    ///   what was there before. [method VLCTrack.get_info] answers the whole
    ///   struct in one call.
    /// - libvlc reports this for a track that is being played as well as for one
    ///   that is not.
    /// - This signal carries arguments; see the note in [signal track_added].
    #[signal]
    fn track_updated(track_type: i32, id: GString);
    /// Emitted when a track of this input becomes selected.
    ///
    /// # Parameters
    /// - [param track_type] the type of the track that was selected.
    /// - [param id] the id of the track that was selected.
    ///
    /// # Note
    /// - One call that changes several tracks -- [method select_tracks] with a new
    ///   set, a media opening and libvlc picking its defaults -- produces one
    ///   signal per track, in the order libvlc applied them, mixed with
    ///   [signal track_unselected] for the ones that left.
    /// - Selecting a track that is already selected reports nothing: libvlc only
    ///   raises the event when the state moves.
    /// - This signal carries arguments; see the note in [signal track_added].
    #[signal]
    fn track_selected(track_type: i32, id: GString);
    /// Emitted when a track of this input stops being selected.
    ///
    /// # Parameters
    /// - [param track_type] the type of the track that was unselected.
    /// - [param id] the id it had.
    ///
    /// # Note
    /// - libvlc raises it for each track that leaves, so replacing a selection of
    ///   three with one of one reports three of these and then that one
    ///   [signal track_selected].
    /// - The track is still there, and [method get_track_from_id] still finds it:
    ///   this is not [signal track_removed]. libvlc has no call that unloads a
    ///   track.
    /// - This signal carries arguments; see the note in [signal track_added].
    #[signal]
    fn track_unselected(track_type: i32, id: GString);

    // ── chapters and titles ──

    /// Emitted when the chapter being played changes.
    ///
    /// # Parameters
    /// - [param chapter] the number of the chapter now playing, counted from `0`
    ///   inside whichever title is selected.
    ///
    /// # Note
    /// - The number is all libvlc sends. It holds the title index and the
    ///   chapter's own name where it raises this and passes on neither, so a
    ///   listener that has to name the chapter asks [method get_title] and
    ///   [method get_full_chapter_descriptions] for them.
    /// - One change can be reported more than once, and not in the order it
    ///   happened: the number follows the input's demuxer, which reads ahead of
    ///   playback. Measured with `test/media/h264_64x64_4s_4chapters.mp4`, a
    ///   single [method set_chapter]`(2)` arrived as `0`, `1`, `2`, `3` within
    ///   half a second, while the input had already reported `1` and `2` before
    ///   the jump. Treat this as "the number moved"; read [method get_chapter]
    ///   for where it stands.
    /// - A chapter number is counted inside a title, and every title numbers its
    ///   own chapters from `0`, so on an input with several titles this alone does
    ///   not say where playback is. This binding keeps no record of the previous
    ///   title to work it out from, by design.
    /// - [method set_chapter] and [method next_chapter] return nothing and report
    ///   nothing when they do not apply, whether the index is out of range or the
    ///   media has no chapters at all. This signal is the only report that a
    ///   change happened, which means its absence is not proof that one did not.
    #[signal]
    fn chapter_changed(chapter: i32);

    /// Emitted when the titles an input offers change.
    ///
    /// # Note
    /// - It carries nothing on purpose: libvlc raises it with an empty payload,
    ///   and the answer is to ask [method get_full_title_descriptions] again.
    /// - This is what makes that call start answering. Titles are published by a
    ///   running input, so before playback there is no list to read and nothing
    ///   announces that one exists -- this signal does.
    /// - A *different* title being selected from that list is [signal
    ///   title_selection_changed], which is a separate thing and can follow this
    ///   one.
    #[signal]
    fn title_list_changed();

    /// Emitted when the selected title changes.
    ///
    /// # Parameters
    /// - [param index] the index of the title that is now selected, counted from
    ///   `0`. [method get_title] reports the same number.
    /// - [param title] that title's description, with exactly the keys one entry
    ///   of [method get_full_title_descriptions] has: `name`, `duration` and
    ///   `flags`.
    ///
    /// # Note
    /// - The description is copied out of the event and handed over as an
    ///   ordinary dictionary. libvlc raises this with a pointer into the frame it
    ///   is calling from, and the name inside it is borrowed rather than owned,
    ///   so the copy is the point: nothing here outlives the call that received
    ///   it.
    /// - A payload that names no title is not reported at all: an event saying a
    ///   title was selected while pointing at none is not something to hand on,
    ///   so this signal never carries an empty dictionary. The pinned runtime
    ///   does not send one either.
    #[signal]
    fn title_selection_changed(index: i32, title: VarDictionary);

    // ── media / texture / GPU ──

    /// Assigns the media to play, clearing the picture this player was showing
    /// first when it is a different media (see [signal frame_cleared]).
    #[func]
    pub fn set_media(&mut self, media: Option<Gd<VlcMedia>>) {
        // Assigning a media is the moment a caller can ask this player to let go
        // of what it is showing: the frames arriving between here and the new
        // media's first one belong to what is being replaced. Assigning `null`
        // is not that -- libvlc is handed nothing to change to, so it keeps
        // playing what it has and the picture stays up.
        let replaces = match (&self.media, &media) {
            (Some(current), Some(next)) => current.instance_id() != next.instance_id(),
            (None, Some(_)) => true,
            _ => false,
        };
        self.media = media;
        if replaces {
            self.clear_frame();
        }
        self.update_media();
    }

    /// Drops the picture this player is showing and waits for the video output
    /// of the media that replaces it.
    fn clear_frame(&mut self) {
        let had_picture = self.frame.is_some() || self.gpu_was_showing();
        self.frame = None;
        self.awaiting_output = true;
        // Blanked in place rather than swapped for another texture: `get_texture`
        // hands this object out, and a caller that kept it has to see the picture
        // go away as well. 1 by 1 and transparent, so nothing is drawn from it.
        if let Some(mut blank) = Image::create_empty(1, 1, false, image::Format::RGBA8) {
            blank.fill(Color::TRANSPARENT_BLACK);
            self.texture.set_image(&blank);
        }
        #[cfg(all(feature = "gpu", windows))]
        if let Some(importer) = self.gpu_importer.clone() {
            // The render-device texture is left alone: it is off the screen while
            // this is set, and the importer keeps writing to it every frame
            // regardless. The software texture stands in for it until the
            // importer builds the texture of the output that follows.
            self.texture_rect
                .set_texture(&self.texture.clone().upcast::<Texture2D>());
            self.gpu_picture_hidden = true;
            self.gpu_hidden_rid = importer.current_dst_rid();
        }
        if had_picture {
            self.signals().frame_cleared().emit();
        }
    }

    /// Whether the GPU output path has put a frame on screen. Always `false` in
    /// builds without it.
    #[cfg(all(feature = "gpu", windows))]
    fn gpu_was_showing(&self) -> bool {
        self.gpu_importer
            .as_ref()
            .is_some_and(|importer| importer.frames_copied.load(Ordering::SeqCst) > 0)
    }

    #[cfg(not(all(feature = "gpu", windows)))]
    fn gpu_was_showing(&self) -> bool {
        false
    }

    /// Puts the GPU picture back up once the importer has a destination texture
    /// that was not there when the picture was cleared.
    #[cfg(all(feature = "gpu", windows))]
    fn restore_gpu_picture(&mut self) {
        if !self.gpu_picture_hidden {
            return;
        }
        let Some(importer) = self.gpu_importer.clone() else {
            return;
        };
        if importer.current_dst_rid() == self.gpu_hidden_rid {
            return;
        }
        let drd = importer
            .texture_2drd
            .lock()
            .expect("texture_2drd poisoned")
            .clone();
        self.texture_rect.set_texture(&drd.upcast::<Texture2D>());
        self.gpu_picture_hidden = false;
    }

    /// This player's texture, blank until playback puts a frame in it.\
    /// It is the object the control draws with, and it is the same object for the whole life of this player: a media change blanks it in place rather than swapping a new one in, so a caller that kept what this handed out sees the picture go away as well (see [signal frame_cleared]). While it is blank it is 1 by 1 and transparent, whatever the size of the video was.
    ///
    /// # Note
    /// - On Windows with [member force_hardware] this is not the texture on screen: the GPU output has a render-device texture of its own, and this one is what stands in for it while the picture is blank.
    #[func]
    fn get_texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    /// The most recent frame, as an [Image].\
    /// This is ours rather than libvlc's: the frames LibVLC delivers are already images on the software output path, and this hands back the last one instead of making a caller read the texture, which not every rendering driver allows.
    ///
    /// # What it is for, and what it is not
    /// It is the frame **before** Godot draws it: the same pixels the texture received, at the size the decoder produced. Subtitles are part of those pixels -- LibVLC's video output composites them -- so two frames taken at the same point of a media differ by whether a subtitle was shown, which is what makes this usable as evidence that a subtitle was rendered rather than merely attached.
    ///
    /// This is not a screenshot facility and not a snapshot API: it does not capture on demand, does not decode anything, and does not look at the GPU path. On Windows with [member force_hardware] the GPU output is driving, its frames live in a texture this binding never reads back, and this returns `null` -- as it does before the first frame of any playback, and as it does again from [signal frame_cleared] until the media that replaced the old one produces a frame.
    ///
    /// # Returns
    /// the last software-path frame, or `null` when the GPU output is running or no
    /// frame has arrived yet.
    #[func]
    fn get_frame(&self) -> Option<Gd<Image>> {
        self.frame.clone()
    }

    /// Whether the GPU output backend is currently driving this player.
    /// Reflects the *actual* state after `try_init_gpu_backend()`: if
    /// [member force_hardware] was set but init failed and we fell back to the
    /// software path, this returns `false`.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn is_gpu_output_active(&self) -> bool {
        self.gpu_backend.is_some()
    }

    #[cfg(not(all(feature = "gpu", windows)))]
    #[func]
    fn is_gpu_output_active(&self) -> bool {
        false
    }

    // ── debug functions ──

    /// How many libvlc event handlers are attached and not yet detached, over
    /// every object in this process.
    ///
    /// A freed wrapper has to leave none of its own behind, whatever holds the
    /// libvlc object it was wrapping, so this reads zero once everything a test
    /// built has been freed. A static method: the question is about the
    /// process, not about one player.
    #[func]
    fn _debug_outstanding_attachments() -> i64 {
        crate::vlc_event_attachments::outstanding()
    }

    /// Pop the pending GPU output event from the mailbox, returning the
    /// texture's `(width, height)` or `(0, 0)` if empty. Drains the mailbox.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_pop_gpu_event(&self) -> Vector2i {
        let Some(mailbox) = self.gpu_mailbox.as_ref() else {
            return Vector2i::new(0, 0);
        };
        match mailbox.take() {
            Some(e) => Vector2i::new(e.width as i32, e.height as i32),
            None => Vector2i::new(0, 0),
        }
    }

    /// True when the GPU backend successfully initialized for this player.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_gpu_active(&self) -> bool {
        self.gpu_backend.is_some()
    }

    /// True while the control is drawing something other than the GPU texture: a media
    /// change points it back at the blank software texture, and the new video output points
    /// it at a texture of its own again. False when the GPU backend isn't active.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_gpu_picture_hidden(&self) -> bool {
        let Some(importer) = self.gpu_importer.as_ref() else {
            return false;
        };
        let drd = importer
            .texture_2drd
            .lock()
            .expect("texture_2drd poisoned")
            .clone();
        match self.texture_rect.get_texture() {
            Some(shown) => shown.instance_id() != drd.instance_id(),
            None => true,
        }
    }

    /// Number of per-frame `copy_and_sync` invocations completed by the
    /// importer. 0 if the GPU backend isn't active.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_frames_copied(&self) -> i64 {
        match &self.gpu_importer {
            None => 0,
            Some(t) => t.frames_copied.load(std::sync::atomic::Ordering::SeqCst) as i64,
        }
    }

    /// libvlc callback hit counts as `(update_output, swap, make_current)`.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_callback_counts(&self) -> Vector3i {
        let Some(b) = self.gpu_backend.as_ref() else {
            return Vector3i::new(0, 0, 0);
        };
        Vector3i::new(
            b.update_output_calls
                .load(std::sync::atomic::Ordering::SeqCst) as i32,
            b.swap_calls.load(std::sync::atomic::Ordering::SeqCst) as i32,
            b.make_current_calls
                .load(std::sync::atomic::Ordering::SeqCst) as i32,
        )
    }

    /// Average RGBA of the GPU destination texture, read back via
    /// `RenderingDevice::texture_get_data`. `Color(0,0,0,0)` when GPU isn't
    /// active or no destination is bound.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_dst_pixel_avg(&self) -> Color {
        let Some(importer) = self.gpu_importer.as_ref() else {
            return Color::from_rgba(0.0, 0.0, 0.0, 0.0);
        };
        let rid = match importer.current_dst_rid() {
            Some(r) => r,
            None => return Color::from_rgba(0.0, 0.0, 0.0, 0.0),
        };
        let mut rd = match RenderingServer::singleton().get_rendering_device() {
            Some(rd) => rd,
            None => return Color::from_rgba(0.0, 0.0, 0.0, 0.0),
        };
        let bytes = rd.texture_get_data(rid, 0);
        if bytes.is_empty() {
            return Color::from_rgba(0.0, 0.0, 0.0, 0.0);
        }
        let mut sums = [0u64; 4];
        let mut count: u64 = 0;
        for chunk in bytes.as_slice().as_chunks::<4>().0 {
            sums[0] += chunk[0] as u64;
            sums[1] += chunk[1] as u64;
            sums[2] += chunk[2] as u64;
            sums[3] += chunk[3] as u64;
            count += 1;
        }
        if count == 0 {
            return Color::from_rgba(0.0, 0.0, 0.0, 0.0);
        }
        let denom = count as f32 * 255.0;
        Color::from_rgba(
            sums[0] as f32 / denom,
            sums[1] as f32 / denom,
            sums[2] as f32 / denom,
            sums[3] as f32 / denom,
        )
    }

    /// Returns `{godot_luid, d3d11_luid}` when the rendering driver is D3D12
    /// and the LUIDs match, or `{error}` otherwise.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_get_adapter_luids(&self) -> VarDictionary {
        use gpu_d3d11::adapter::{dxgi_adapter_luid_for, godot_d3d12_luid};
        let mut dict = VarDictionary::new();
        match godot_d3d12_luid() {
            Err(e) => {
                let _ = dict.insert("error", e.to_string());
            }
            Ok(godot_luid) => {
                let _ = dict.insert("godot_luid", godot_luid);
                match dxgi_adapter_luid_for(godot_luid) {
                    Ok(d3d11_luid) => {
                        let _ = dict.insert("d3d11_luid", d3d11_luid);
                    }
                    Err(e) => {
                        let _ = dict.insert("error", e.to_string());
                    }
                }
            }
        }
        dict
    }

    // ── property setters ──

    #[func]
    fn set_stretch_mode(&mut self, stretch_mode: StretchMode) {
        self.stretch_mode = stretch_mode;
        self.update_stretch_mode();
    }

    #[func]
    fn set_volume_db(&mut self, volume_db: f32) {
        self.volume_db = volume_db;
        self.update_volume_db();
    }

    #[func]
    fn set_mix_target(&mut self, mix_target: MixTarget) {
        self.mix_target = mix_target;
        self.update_mix_target();
    }

    #[func]
    fn set_bus(&mut self, bus: StringName) {
        self.bus = bus;
        self.update_bus();
    }

    // ── playback controls ──

    /// Can this media player be paused?
    ///
    /// # Note
    /// `false` is also what this answers before a media opens and after it stops,
    /// so on its own it cannot tell "cannot be paused" from "nothing to pause".
    ///
    /// # Return values
    /// - `true` media player can be paused
    /// - `false` media player cannot be paused
    #[func]
    fn can_pause(&self) -> bool {
        unsafe { libvlc_media_player_can_pause(self.player_ptr) }
    }

    /// Get the A to B loop: the five values libvlc reports for it.
    ///
    /// # Returns
    /// a dictionary that always carries these five keys:
    /// - `status`: int, [constant ABLOOP_NONE], [constant ABLOOP_A] or
    ///   [constant ABLOOP_B] -- only [constant ABLOOP_B] is a loop that runs
    /// - `a_time`: int, milliseconds, or `-1`
    /// - `a_pos`: float, `0.0`-`1.0`, or `-1`
    /// - `b_time`: int, milliseconds, or `-1`
    /// - `b_pos`: float, `0.0`-`1.0`, or `-1`
    ///
    /// # Note
    /// - `status` decides which of the four values mean anything: the A pair from
    ///   [constant ABLOOP_A] up, the B pair only at [constant ABLOOP_B]. The rest
    ///   read `-1`, and not because libvlc says so: its own values for them are
    ///   uninitialised memory, since the C API writes all four out-parameters
    ///   whether or not they apply. Nothing uninitialised is passed on here.
    /// - The units are those of the entry point that set the loop: one from
    ///   [method set_abloop_time] reports its times and `0` for its positions, and
    ///   one from [method set_abloop_position] reports the fractions and `-1` for
    ///   its times.
    /// - A loop that has been lost -- [method stop_async], a media that ended, a
    ///   replaced [member media] -- reports [constant ABLOOP_NONE] here, and that
    ///   is the only way to notice that it is gone. See [method set_abloop_time].
    /// - libvlc raises no event when a loop is set, cleared or entered, so this
    ///   has to be asked rather than listened for.
    ///   [method get_abloop_status] answers the status alone, without building the
    ///   dictionary.
    #[func]
    fn get_abloop(&self) -> VarDictionary {
        let loop_state = self.ab_loop();
        let mut dict = VarDictionary::new();
        dict.set("status", loop_state.status);
        dict.set("a_time", loop_state.a_time);
        dict.set("a_pos", loop_state.a_pos);
        dict.set("b_time", loop_state.b_time);
        dict.set("b_pos", loop_state.b_pos);
        dict
    }

    /// Get the A to B loop status alone: which parts of it are set.
    ///
    /// # Returns
    /// [constant ABLOOP_NONE] when there is no loop, [constant ABLOOP_A] when
    /// only the A point is, [constant ABLOOP_B] when both are -- the state a loop
    /// has to be in to run.
    ///
    /// # Note
    /// This is a convenience over [method get_abloop] for the question that gets
    /// asked most: it reads the same thing, so the two cannot disagree, and it
    /// costs no dictionary for the callers that ask once per frame.
    #[func]
    fn get_abloop_status(&self) -> i32 {
        self.ab_loop().status
    }

    /// The last buffering percentage libvlc reported, in `0`-`100`, or `0` if it
    /// has reported nothing yet.
    ///
    /// This is the value [signal buffering] carries, cached so that a script
    /// which connects late -- or polls instead of listening -- still has it.
    /// "Is the player buffering *right now*" is a different question: ask
    /// [method get_state] for [constant STATE_BUFFERING].
    ///
    /// # Returns
    /// the last reported percentage, or `0` before the first report.
    #[func]
    fn get_buffering_percent(&self) -> f32 {
        // `max` is what turns [NO_BUFFERING_REPORT] into the documented `0`.
        f32::from_bits(self.buffering_percent.load(Ordering::Relaxed)).max(0.0)
    }

    /// Get movie chapter.
    ///
    /// # Returns
    /// chapter number currently playing, or -1 if there is no media.
    ///
    /// # Note
    /// - The number is the one libvlc's own `ChapterChanged` events carry, which
    ///   is where the input's demuxer has read to -- and a demuxer reads ahead of
    ///   playback. Measured with `test/media/h264_64x64_4s_4chapters.mp4`: the
    ///   events for chapters `1` and `2` arrived while a test that had just
    ///   started playing was still waiting for the title list, and one
    ///   [method set_chapter]`(2)` moved the reporting through `0`, `1`, `2`, `3`
    ///   inside half a second. Read it as "the chapter the input is at", not as
    ///   "the chapter on screen".
    /// - The number is counted from `0` inside whichever title is selected, and
    ///   every title numbers its own chapters from `0`.
    /// - A media that has no chapter list still has an answer here: while an input
    ///   is running this is `0`, not `-1`. libvlc keeps `-1` for having no input at
    ///   all -- `vlc_player_GetSelectedChapterIdx` answers it in that case, and
    ///   otherwise returns a field that starts at `0` -- so on the demo's own movie,
    ///   an MP4 with no chapter table, this reads `0` while [method
    ///   get_full_chapter_descriptions] returns an empty array. The list is what
    ///   tells "the chapter list has not arrived" from "there is no chapter list".
    #[func]
    fn get_chapter(&self) -> i32 {
        unsafe { libvlc_media_player_get_chapter(self.player_ptr) }
    }

    /// Get movie chapter count.
    ///
    /// # Returns
    /// number of chapters in movie, or -1.
    #[func]
    fn get_chapter_count(&self) -> i32 {
        unsafe { libvlc_media_player_get_chapter_count(self.player_ptr) }
    }

    /// Get title chapter count.
    ///
    /// # Parameters
    /// - [param title] the index of the title to ask about, counted from `0`.
    ///
    /// # Returns
    /// the number of chapters in that title, or `-1` when there is no title to
    /// ask about -- the player has no media, or the index is out of range.
    ///
    /// # Note
    /// - **A negative `title` is refused here instead of being passed on.**
    ///   libvlc's implementation opens with `assert(i_title >= 0)`
    ///   (`lib/media_player.c`, `libvlc_media_player_get_chapter_count_for_title`)
    ///   and that assert is live in the runtime this binds: the pinned build is
    ///   configured without `--disable-debug`, which is the switch that defines
    ///   `NDEBUG`. Passing a negative index through aborts the process; libvlc's
    ///   own header promises a `-1` for the same call. This is the one argument
    ///   in this binding that is refused rather than forwarded, and `-1` is what
    ///   it answers instead.
    /// - Chapter *names* are [method get_full_chapter_descriptions], and title
    ///   flags are [method get_full_title_descriptions].
    #[func]
    fn get_chapter_count_for_title(&self, title: i32) -> i32 {
        if title < 0 {
            return -1;
        }
        unsafe { libvlc_media_player_get_chapter_count_for_title(self.player_ptr, title) }
    }

    /// Get the full description of the chapters of one title.
    ///
    /// This is the call that carries a chapter's *name*: [method get_chapter] and
    /// [method get_chapter_count] only count, so a chapter list built from those
    /// reads "Chapter 1" through "Chapter N" and shows nothing the file declared.
    ///
    /// # Parameters
    /// - [param title] the index of the title to ask about, or `-1` for the one
    ///   that is selected now. An index that is out of range is not an error: it
    ///   is one of the ways to get an empty answer.
    ///
    /// # Returns
    /// one dictionary per chapter, in playback order, each with the same three
    /// keys:
    /// - `name`: the chapter's name, and it is never empty. libvlc makes one up
    ///   when the file does not name a chapter -- `seekpoint_GetName` in
    ///   `src/player/title.c` answers `Chapter N` -- and when even that fails it
    ///   drops the title list rather than hand out a nameless entry. The name is
    ///   guaranteed by how the list is built, not by the file.
    /// - `time_offset`: where the chapter starts, in milliseconds from the start
    ///   of its title.
    /// - `duration`: how long it lasts, in milliseconds.
    ///
    /// An empty array means there is nothing to describe, and that is the answer
    /// for every way of getting one: a player with no media, a media that has not
    /// started playing (see the note), an index that is not there, a failed call,
    /// and a title that lists no chapters. libvlc keeps the last of those apart --
    /// that one alone comes back as `0`, every other one as `-1` -- and this
    /// binding folds them together, because a caller acts on all of them the same
    /// way: there is no list. Nothing is reported as a chapter that libvlc did not
    /// report.
    ///
    /// # Note
    /// - **Chapters exist only while the media is playing.** They are part of the
    ///   title list an input publishes once it is running, so this answers an
    ///   empty array before [method play] has got that far -- and parsing is not
    ///   enough: `libvlc_media_parse` never reports titles or chapters at all.
    ///   [signal title_list_changed] is what says the list has arrived.
    /// - `duration` is computed rather than stored: libvlc takes it from the start
    ///   of the next chapter, and for the last chapter from the length of the
    ///   title. When that length is unknown the last chapter's duration comes out
    ///   **negative**. It is reported as it is rather than clamped, because a
    ///   clamped `0` would read as a real length; treat a negative one as
    ///   unknown.
    /// - The array libvlc allocates is freed before this returns: there is no
    ///   release call to make from GDScript, and no way to leak it.
    /// - The `0` that libvlc reserves for "the title exists and lists no chapters"
    ///   is written down rather than exercised: no format used here produces one.
    ///   An MP4 without a chapter list has no title to ask about either, so it
    ///   takes the `-1` path -- `test/media/h264_64x64_1s.mp4` in the acceptance
    ///   tests is exactly that case.
    ///
    /// # See also
    /// - [method VLCTrack.get_info] for the same "one call, one dictionary"
    ///   shape on the track side.
    #[func]
    fn get_full_chapter_descriptions(&self, title: i32) -> Array<VarDictionary> {
        let mut entries: *mut *mut libvlc_chapter_description_t = ptr::null_mut();
        let count = unsafe {
            libvlc_media_player_get_full_chapter_descriptions(self.player_ptr, title, &mut entries)
        };
        unsafe { dictionaries(entries, count) }
    }

    /// Get the current movie length (in ms).
    ///
    /// # Note
    /// libvlc's own header promises `-1` when there is no media, but what it
    /// returns is `0` -- the same "unknown" that stops [signal length_changed]
    /// from being emitted at all.
    ///
    /// # Returns
    /// the movie length (in ms), or `0` while it is unknown.
    #[func]
    fn get_length(&self) -> i64 {
        unsafe { libvlc_media_player_get_length(self.player_ptr) }
    }

    /// Get movie position as percentage between 0.0 and 1.0.
    ///
    /// # Note
    /// Unlike [method get_time] and [method get_length], this one really does
    /// report `-1.0` when there is nothing to report; [signal position_changed]
    /// never carries that value.
    ///
    /// # Returns
    /// movie position, or -1. in case of error.
    #[func]
    fn get_position(&self) -> f64 {
        unsafe { libvlc_media_player_get_position(self.player_ptr) }
    }

    /// Get the requested movie play rate.
    ///
    /// # Warning
    /// Depending on the underlying media, the requested rate may be different from the real playback rate.
    ///
    /// # Returns
    /// movie play rate.
    #[func]
    fn get_rate(&self) -> f32 {
        unsafe { libvlc_media_player_get_rate(self.player_ptr) }
    }

    /// Get current movie state.
    ///
    /// # Returns
    /// the current state of the media player([constant STATE_PLAYING], [constant STATE_PAUSED], ...)
    #[func]
    pub fn get_state(&self) -> i32 {
        unsafe { libvlc_media_player_get_state(self.player_ptr) as i32 }
    }

    /// Get the current movie time (in ms).
    ///
    /// # Note
    /// `0` while there is nothing to report. libvlc's header promises `-1` here
    /// too, and its implementation returns the same "unknown" as its length.
    ///
    /// # Returns
    /// the movie time (in ms), or `0` while it is unknown.
    #[func]
    fn get_time(&self) -> i64 {
        unsafe { libvlc_media_player_get_time(self.player_ptr) }
    }

    /// Get movie title.
    ///
    /// # Returns
    /// title number currently playing, or -1.
    #[func]
    fn get_title(&self) -> i32 {
        unsafe { libvlc_media_player_get_title(self.player_ptr) }
    }

    /// Get movie title count.
    ///
    /// # Returns
    /// title number count, or -1.
    #[func]
    fn get_title_count(&self) -> i32 {
        unsafe { libvlc_media_player_get_title_count(self.player_ptr) }
    }

    /// Get the full description of the available titles.
    ///
    /// A title is one selectable part of an input: the feature on a DVD, a
    /// subsong in a chiptune file, a part a demuxer publishes separately.
    /// [method get_title_count] says how many there are; [method set_title]
    /// switches between them.
    ///
    /// # Returns
    /// one dictionary per title, in libvlc's order, each with the same three keys:
    /// - `name`: the title's name. This one is always there, and when the file
    ///   declares none libvlc generates it -- see the note.
    /// - `duration`: the title's length in milliseconds, or `0` when libvlc does
    ///   not know it.
    /// - `flags`: [constant TITLE_MENU], [constant TITLE_INTERACTIVE], both of
    ///   them, or `0` for a plain title.
    ///
    /// An empty array means there is nothing to describe: a player with no media,
    /// a media that is not playing (see the note), or a failed call -- libvlc
    /// answers `-1` for that last one, and this folds it in.
    ///
    /// # Note
    /// - **An MP4 only has titles when it has chapters.** The demuxer builds its
    ///   single title out of the file's chapter list, so
    ///   `test/media/h264_64x64_4s_4chapters.mp4` answers one title with its four
    ///   chapters under it, and `test/media/h264_64x64_1s.mp4`, which has no
    ///   chapter list at all, answers no title list at all. Chapters are never
    ///   titles.
    /// - An empty array therefore rarely means "this media has no titles": for the
    ///   formats here it means the file declared no chapters, or the list has not
    ///   arrived yet. [signal title_list_changed] is what settles which.
    /// - A name libvlc generated for itself is `Title 0`, with the length in
    ///   brackets appended when libvlc knows it -- `Title 0 [00:04]` for the sample
    ///   above. That is libvlc's `vlc_tick_to_str`, which writes `MM:SS` and only
    ///   grows to `H:MM:SS` past the hour; the wording is libvlc's, and this
    ///   binding changes none of it. A name that came from the file arrives as the
    ///   file wrote it.
    /// - **Titles, like chapters, exist only while the media is playing**, for the
    ///   same reason and with the same symptom: an empty array before then.
    /// - Which chapters a title has is [method get_full_chapter_descriptions]'
    ///   job, and a title whose flags include [constant TITLE_MENU] is what
    ///   [method navigate] directs.
    /// - The array libvlc allocates is freed before this returns.
    #[func]
    fn get_full_title_descriptions(&self) -> Array<VarDictionary> {
        let mut entries: *mut *mut libvlc_title_description_t = ptr::null_mut();
        let count = unsafe {
            libvlc_media_player_get_full_title_descriptions(self.player_ptr, &mut entries)
        };
        unsafe { dictionaries(entries, count) }
    }

    /// Get the track list for one type.\
    /// The track list can be used to get track information and to select specific tracks.
    ///
    /// # Note
    /// You need to call [method VLCMedia.parse_request] or play the media at least once before calling this function. Not doing this will result in an empty list.\
    /// This track list is a snapshot of the current tracks when this function is called. If a track is updated after this call, the user will need to call this function again to get the updated track.
    ///
    /// # Note
    /// - It is built anew on every call and reads the player's live state, so
    ///   `selected` here is not a cached answer -- but a selection made a moment
    ///   ago may not have landed yet, because libvlc queues it to its input
    ///   thread. [signal track_selected] and [signal track_unselected] are what
    ///   say when to ask again.
    /// - These are the tracks libvlc can select: they came from the player, which
    ///   is the half of the track API that carries the `es_id` selection acts on.
    ///   A tracklist taken from a [VLCMedia] cannot be selected from.
    /// - How many can be selected at once is libvlc's decision, not one of these
    ///   parameters': at most one audio track, at most two text tracks, and any
    ///   number of video tracks.
    ///
    /// # Parameters
    /// - [param track_type] type of the track list to request ([constant VLCTrack.TYPE_AUDIO], [constant VLCTrack.TYPE_VIDEO],...)
    /// - [param selected] filter only selected tracks if true (return all tracks, even selected ones if false)
    ///
    /// # Returns
    /// a valid [VLCTrackList] object, or `null` in case of error, if there is no track for a category, the returned list will have a size of 0.
    #[func]
    fn get_tracklist(&self, track_type: i32, selected: bool) -> Option<Gd<VlcTrackList>> {
        let ptr =
            unsafe { libvlc_media_player_get_tracklist(self.player_ptr, track_type, selected) };
        VlcTrackList::from_ptr(ptr, true)
    }

    /// The track of one type that is selected right now, if there is one.
    ///
    /// # Parameters
    /// - [param track_type] [constant VLCTrack.TYPE_AUDIO], [constant VLCTrack.TYPE_VIDEO],...
    ///
    /// # Returns
    /// a [VLCTrack] the caller owns, or `null`.
    ///
    /// # Note
    /// - `null` means one of two things this call cannot tell apart: nothing of
    ///   that type is selected, or the player has no input yet (before a media
    ///   opens, and after it stops). [method get_state] separates them.
    /// - **A type can have several tracks selected at once** -- up to two text
    ///   tracks, and any number of video ones -- and then this answers with one of
    ///   them. libvlc's own header says so and points at
    ///   [method get_tracklist] with `selected` set: that is the call that lists
    ///   all of them.
    /// - Unlike a track from a tracklist snapshot, this one is read at the moment
    ///   you ask, and it carries no selection flag of its own to go stale.
    #[func]
    fn get_selected_track(&self, track_type: i32) -> Option<Gd<VlcTrack>> {
        let ptr = unsafe { libvlc_media_player_get_selected_track(self.player_ptr, track_type) };
        if ptr.is_null() {
            None
        } else {
            // The reference is already the caller's -- libvlc documents these two
            // getters as returning one that has to be released -- so unlike a
            // tracklist entry this one is not held again here. `VlcTrack`'s Drop
            // releases exactly the one reference it was handed.
            Some(VlcTrack::from_ptr(ptr, true))
        }
    }

    /// The track that carries this string identifier, if there is one right now.
    ///
    /// # Parameters
    /// - [param id] a track's [method VLCTrack.get_id].
    ///
    /// # Returns
    /// a [VLCTrack] the caller owns, or `null` when no track of the current input
    /// has that id.
    ///
    /// # Note
    /// - The id has to come from **this player's** tracklist. A tracklist taken
    ///   from a [VLCMedia] numbers its tracks differently and this call never
    ///   matches one of them.
    /// - This is the other half of what [method VLCTrack.get_id] promises: save an
    ///   id, hand it back here later. [method VLCTrack.get_info] reports
    ///   `id_stable`, which is what that promise rests on -- `true` means the id
    ///   came from the demuxer's own track number rather than from a counter
    ///   libvlc invented for this playback, so it is worth saving. It is not a
    ///   guarantee that the same file names the same track the same way next time:
    ///   that is the demuxer's business, and nothing here checks it.
    /// - Only the current input is searched, and only its video, audio and text
    ///   tracks: an id saved before a media change, or one for a category this
    ///   input has none of, answers `null`.
    #[func]
    fn get_track_from_id(&self, id: GString) -> Option<Gd<VlcTrack>> {
        let id = cstring_from_gstring(id);
        let ptr = unsafe { libvlc_media_player_get_track_from_id(self.player_ptr, id.as_ptr()) };
        if ptr.is_null() {
            None
        } else {
            Some(VlcTrack::from_ptr(ptr, true))
        }
    }

    /// is_playing
    ///
    /// # Return values
    /// - `true` media player is playing
    /// - `false` media player is not playing
    #[func]
    fn is_playing(&self) -> bool {
        unsafe { libvlc_media_player_is_playing(self.player_ptr) }
    }

    /// Is this media player seekable?
    ///
    /// # Note
    /// `false` is also what this answers before a media opens and after it stops,
    /// so on its own it cannot tell "not seekable" from "nothing to seek in".
    ///
    /// # Return values
    /// - `true` media player can seek
    /// - `false` media player cannot seek
    #[func]
    fn is_seekable(&self) -> bool {
        unsafe { libvlc_media_player_is_seekable(self.player_ptr) }
    }

    /// Navigate through DVD Menu.
    ///
    /// # Parameters
    /// [param navigate] the Navigation mode([constant NAVIGATE_ACTIVATE], [constant NAVIGATE_UP],...)
    #[func]
    fn navigate(&mut self, navigate: u32) {
        unsafe { libvlc_media_player_navigate(self.player_ptr, navigate) }
    }

    /// Set next chapter (if applicable)
    #[func]
    fn next_chapter(&mut self) {
        unsafe { libvlc_media_player_next_chapter(self.player_ptr) }
    }

    /// Display the next frame (if supported)
    #[func]
    fn next_frame(&mut self) {
        unsafe { libvlc_media_player_next_frame(self.player_ptr) }
    }

    /// Toggle pause (no effect if there is no media)
    #[func]
    fn pause(&mut self) {
        unsafe { libvlc_media_player_pause(self.player_ptr) }
    }

    /// Play.
    ///
    /// # Returns
    /// A libvlc status code: `0` once the player has been started,
    /// `VLC_EGENERIC` (`-2147483648`) when there is no media to start, or
    /// `-ENOMEM` when the input could not be allocated. The `-1` that libvlc's
    /// own header documents here is never returned; what libvlc returns is what
    /// this returns.
    ///
    /// # Warning
    /// - It is not "the media can play". A media that cannot be opened at all --
    ///   a file that is not there, a URL that answers 404 -- is accepted here and
    ///   fails afterwards, so that failure arrives as [signal error] instead of
    ///   as a return value.
    #[func]
    fn play(&mut self) -> i32 {
        unsafe { libvlc_media_player_play(self.player_ptr) }
    }

    /// Set previous chapter (if applicable)
    #[func]
    fn previous_chapter(&mut self) {
        unsafe { libvlc_media_player_previous_chapter(self.player_ptr) }
    }

    /// Remove the A to B loop from the current media.
    ///
    /// # Returns
    /// `0` when libvlc removed it, `-1` when it refused.
    ///
    /// # Warning
    /// - It only works while the input can seek, which in practice means **while
    ///   the media is playing**. Called before playback -- including on a loop
    ///   that was set before playback, which libvlc accepts -- it answers `-1` and
    ///   removes nothing: the loop stays on the input and runs when the media
    ///   plays. The only other way to be rid of it is to let the input go, with
    ///   [method stop_async] or a new [member media].
    /// - It says nothing about a loop on a *different* input: a script that
    ///   stopped playback and started it again has a fresh input and no loop on
    ///   it, whatever this answered before.
    #[func]
    fn reset_abloop(&mut self) -> i32 {
        unsafe { libvlc_media_player_reset_abloop(self.player_ptr) }
    }

    /// Select one track, replacing the whole selection of its type.
    ///
    /// This is `libvlc_media_player_select_track`, whose own documentation says to
    /// use [method select_tracks] for more than one.
    ///
    /// # Parameters
    /// - [param track] a track from [method get_tracklist], [method get_selected_track]
    ///   or [method get_track_from_id]. A track from [method VLCMedia.get_tracklist]
    ///   is refused; see below.
    ///
    /// # Note
    /// - It is queued to libvlc's input thread, so it returns before anything has
    ///   changed: read the selection back through [method get_selected_track] or
    ///   [method get_tracklist] once [signal track_selected] arrives, not on the
    ///   next line.
    /// - It needs a playing input, and does nothing at all without one -- before a
    ///   media is assigned, after a stop. Nothing reports that, in libvlc or here.
    ///   [method select_tracks_by_ids] is the one that can be called first.
    /// - A track from a media descriptor is **refused with an error in the log**.
    ///   libvlc's header asks for a track from the player's tracklist; what it does
    ///   with one from the media's is not a refusal -- the assertion that would
    ///   catch it is compiled out of the shipped build, and the call dereferences
    ///   the `es_id` that track does not have.
    /// - Selecting a track that is already selected changes nothing and emits
    ///   nothing.
    #[func]
    fn select_track(&mut self, track: Gd<VlcTrack>) {
        if !track.bind().from_player {
            godot_error!(
                "godot-vlc: select_track was given a track from a media descriptor, which libvlc cannot select; use a track from VLCMediaPlayer.get_tracklist, or select by its id"
            );
            return;
        }
        unsafe { libvlc_media_player_select_track(self.player_ptr, track.bind().ptr) }
    }

    /// Select a set of tracks of one type, replacing whatever was selected before.
    ///
    /// This is `libvlc_media_player_select_tracks`, and it sets the selection
    /// rather than adding to it: every track of that type that is not in the array
    /// is unselected. An empty array therefore clears the type's selection, which
    /// is what [method unselect_track_type] does.
    ///
    /// # Parameters
    /// - [param track_type] the type the tracks belong to: [constant VLCTrack.TYPE_AUDIO],
    ///   [constant VLCTrack.TYPE_VIDEO] or [constant VLCTrack.TYPE_TEXT].
    /// - [param tracks] the tracks to select, from this player's tracklist.
    ///
    /// # Note
    /// - **libvlc caps this per type, silently**: one audio track, two text
    ///   tracks, any number of video ones. Asking for two audio tracks selects one
    ///   of them and reports nothing anywhere, here or in libvlc. Read the
    ///   selection back if it matters.
    /// - It is queued to libvlc's input thread and needs a playing input, like
    ///   [method select_track]: without one it does nothing at all, silently.
    ///   [method select_tracks_by_ids] is the one that can be called before
    ///   playback.
    /// - A track from a media descriptor is refused with an error in the log, as
    ///   in [method select_track], and the whole call is refused with it -- a set
    ///   is not selected halfway.
    /// - One call produces one signal per track whose state actually changed, in
    ///   arrival order: up to one [signal track_unselected] per track that left the
    ///   selection and one [signal track_selected] per track that joined it. A
    ///   handler that redraws on each of them will redraw several times.
    #[func]
    fn select_tracks(&mut self, track_type: i32, tracks: Array<Gd<VlcTrack>>) {
        let mut pointers: Vec<*const libvlc_media_track_t> = Vec::with_capacity(tracks.len());
        for index in 0..tracks.len() {
            let Some(track) = tracks.get(index) else {
                continue;
            };
            if !track.bind().from_player {
                godot_error!(
                    "godot-vlc: select_tracks was given a track from a media descriptor, which libvlc cannot select; nothing was selected"
                );
                return;
            }
            pointers.push(track.bind().ptr);
        }
        unsafe {
            libvlc_media_player_select_tracks(
                self.player_ptr,
                track_type,
                pointers.as_mut_ptr(),
                pointers.len(),
            )
        }
    }

    /// Select tracks of one type by their string identifiers.
    ///
    /// This is `libvlc_media_player_select_tracks_by_ids`, and it sets the
    /// selection rather than adding to it, as [method select_tracks] does.
    ///
    /// # Parameters
    /// - [param track_type] the type the ids belong to.
    /// - [param ids] one or more [method VLCTrack.get_id] values, separated by
    ///   commas -- `"video/1,video/2"`. An empty string, or one that matches
    ///   nothing, clears the type's selection; that is libvlc's rule, not this
    ///   binding's.
    ///
    /// # Note
    /// - **This is the one selection call that works before playback.** libvlc
    ///   keeps the ids on the player and applies them when the input for the
    ///   current media is created, which is what makes "remember the viewer's
    ///   subtitle choice and restore it next time" possible. It has no effect on
    ///   the media after the next one.
    /// - The per-type caps of [method select_tracks] apply here too, and so does
    ///   the silence: two audio ids select one audio track.
    /// - An id is only worth saving when [method VLCTrack.get_info] reported
    ///   `id_stable` as `true`; see [method get_track_from_id] for what that does
    ///   and does not promise.
    /// - Unlike [method select_tracks] this one takes nothing that could come from
    ///   a media descriptor, so there is nothing to refuse here.
    #[func]
    fn select_tracks_by_ids(&mut self, track_type: i32, ids: GString) {
        let ids = cstring_from_gstring(ids);
        unsafe {
            libvlc_media_player_select_tracks_by_ids(self.player_ptr, track_type, ids.as_ptr())
        }
    }

    /// Play the current media from `a_ms` to `b_ms` over and over.
    ///
    /// The loop is libvlc's own: at the B point the input seeks back to A, so the
    /// media is not re-opened, nothing is re-parsed, and [method get_state] stays
    /// in [constant STATE_PLAYING] across the wrap.
    ///
    /// # Parameters
    /// - [param a_ms] where the loop starts, in milliseconds. `0` is a valid
    ///   start; a negative value is refused.
    /// - [param b_ms] where the loop goes back to [param a_ms], in milliseconds.
    ///   It has to be above [param a_ms]. Past the end of the media it is not an
    ///   error: the wrap is then driven by the end of the media instead, which
    ///   makes `set_abloop_time(0, <a large value>)` a way to loop the whole of a
    ///   media without knowing its length first.
    ///
    /// # Returns
    /// `0` when libvlc took it, `-1` when it refused: [param b_ms] not above
    /// [param a_ms], either of them negative, or no [member media] on this player
    /// yet.
    ///
    /// # Warning
    /// - **The loop belongs to the input, not to the player.** It is gone after
    ///   [method stop_async], after the media ends, and after [member media] is
    ///   replaced, so a script that starts playback again has to set it again.
    ///   Nothing here remembers it, and [method get_abloop_status] is how to
    ///   notice.
    /// - **It needs an input that can seek.** On one that cannot -- a live source
    ///   -- the call still answers `0` and simply never wraps.
    ///   [method is_seekable] is the question to ask, remembering that it answers
    ///   `false` before a media opens too.
    /// - **The B point is a threshold, not an exact end.** Measured on the pinned
    ///   runtime: the wrap lands 0-100 ms after `b_ms`, and a `b_ms` below about
    ///   400 ms loops at about 400 ms whatever was asked for. A short clip cannot
    ///   be looped tightly.
    /// - **Nothing is emitted when the loop wraps.** libvlc has no such event, and
    ///   neither [signal time_changed] nor [signal position_changed] can stand in
    ///   for one: they jump back to A exactly as they do for a seek, so a wrap
    ///   cannot be told apart from one. A progress bar will jump back with the
    ///   picture.
    /// - [method set_abloop_position] is the same setting expressed as
    ///   fractions of the media instead of milliseconds; whichever of the two was
    ///   called last is the loop that runs.
    /// - It can be set before playback -- as soon as [member media] is assigned --
    ///   but not cleared before playback: [method reset_abloop] is refused until
    ///   the media is playing.
    #[func]
    fn set_abloop_time(&mut self, a_ms: i64, b_ms: i64) -> i32 {
        unsafe { libvlc_media_player_set_abloop_time(self.player_ptr, a_ms, b_ms) }
    }

    /// Play the current media over and over between two points, given as
    /// fractions of its length.
    ///
    /// This is [method set_abloop_time] measured in positions rather than in
    /// milliseconds, and the two are one setting: whichever was called last is the
    /// loop that runs. Measured on the pinned runtime for a local file, they
    /// behave the same -- a loop of `0.0`-`0.4` wrapped every ~400 ms, with the
    /// same 0-100 ms of overshoot on the B point.
    ///
    /// # Parameters
    /// - [param a_pos] where the loop starts, `0.0`-`1.0`.
    /// - [param b_pos] where the loop goes back to [param a_pos], `0.0`-`1.0`, and
    ///   above [param a_pos].
    ///
    /// # Returns
    /// `0` when libvlc took it, `-1` when it refused: either value outside
    /// `0.0`-`1.0`, [param b_pos] not above [param a_pos], or no [member media] on
    /// this player yet.
    ///
    /// # Warning
    /// - Everything [method set_abloop_time] warns about holds here too: the loop
    ///   belongs to the input, it cannot be cleared before playback, it needs an
    ///   input that can seek, and no signal announces it.
    /// - What [method get_abloop] reports changes with the entry point: the times
    ///   answer `-1` for a loop set this way, and the positions carry the
    ///   fractions instead.
    /// - libvlc works the wrap out from the length of the media, so a media that
    ///   never reports one is the case to be careful with; the time entry point
    ///   has no such dependency.
    #[func]
    fn set_abloop_position(&mut self, a_pos: f64, b_pos: f64) -> i32 {
        unsafe { libvlc_media_player_set_abloop_position(self.player_ptr, a_pos, b_pos) }
    }

    /// Set movie chapter (if applicable).
    ///
    /// # Parameters
    /// - [param chapter] chapter number to play
    #[func]
    fn set_chapter(&mut self, chapter: i32) {
        unsafe { libvlc_media_player_set_chapter(self.player_ptr, chapter) }
    }

    /// Pause or resume (no effect if there is no media)
    ///
    /// # Parameters
    /// - [param do_pause] play/resume if `false`, pause if `true`
    #[func]
    fn set_pause(&mut self, do_pause: bool) {
        unsafe { libvlc_media_player_set_pause(self.player_ptr, do_pause as c_int) }
    }

    /// Set movie position as percentage between 0.0 and 1.0.\
    /// This has no effect if playback is not enabled. This might not work depending on the underlying input format and protocol.
    ///
    /// # Parameters
    /// - [param pos] the position
    /// - [param fast] prefer fast seeking or precise seeking
    ///
    /// # Returns
    /// 0 on success, -1 on error
    #[func]
    fn set_position(&mut self, pos: f64, fast: bool) -> i32 {
        unsafe { libvlc_media_player_set_position(self.player_ptr, pos, fast) }
    }

    /// Set movie play rate.
    ///
    /// # Parameters
    /// - [param rate] movie play rate to set
    ///
    /// # Returns
    /// -1 if an error was detected, 0 otherwise (but even then, it might not actually work depending on the underlying media protocol)
    #[func]
    fn set_rate(&mut self, rate: f32) -> i32 {
        unsafe { libvlc_media_player_set_rate(self.player_ptr, rate) }
    }

    /// Set the movie time (in ms).\
    /// This has no effect if no media is being played. Not all formats and protocols support this.
    ///
    /// # Parameters
    /// - [param time] the movie time (in ms).
    /// - [param fast] prefer fast seeking or precise seeking
    ///
    /// # Returns
    /// 0 on success, -1 on error
    #[func]
    fn set_time(&mut self, time: i64, fast: bool) -> i32 {
        unsafe { libvlc_media_player_set_time(self.player_ptr, time, fast) }
    }

    /// Move the movie time by a relative amount, in milliseconds.
    ///
    /// `jump_time(-10000)` is what a "rewind ten seconds" button wants: libvlc adds
    /// [param delta_ms] to **its own** current time inside the player, so this does
    /// not race a [method get_time] of the caller's. The seek is precise, the same
    /// as [method set_time] with `fast` false.
    ///
    /// # The edges
    /// - A jump that lands before the start is clamped to the start of the input,
    ///   and that is not an error. The clamp is to libvlc's own "first tick", which
    ///   is one microsecond rather than zero, so what [method get_time] reports
    ///   afterwards is a number very close to zero rather than exactly zero.
    /// - A jump that lands past the end is **not** clamped: libvlc hands the value
    ///   to the demuxer, which goes to the end of the stream. Measured on this
    ///   runtime, playback then runs out of input and ends -- the player reaches
    ///   [constant STATE_STOPPED] with its clock back at `0` -- and this still
    ///   answers `0` either way, so a caller that wants to avoid it has to compare
    ///   with [method get_length] itself.
    /// - With no input -- no media, or nothing played yet -- it does nothing at all
    ///   and still answers `0`: the same "accepted and thrown away" as
    ///   [method set_time].
    /// - An input that cannot seek fails silently. libvlc writes one warning to its
    ///   own log and this still answers `0`, so a caller that needs to know should
    ///   read [method is_seekable] first.
    ///
    /// # Parameters
    /// - [param delta_ms] how far to move, in milliseconds: `10000` is ten seconds
    ///   forward, `-10000` ten seconds back.
    ///
    /// # Returns
    /// `0`. libvlc's header promises `-1 on error`; its implementation returns `0`
    /// on every path, including the ones above where nothing happened.
    #[func]
    fn jump_time(&mut self, delta_ms: i64) -> i32 {
        unsafe { libvlc_media_player_jump_time(self.player_ptr, delta_ms) }
    }

    /// Starts watching the playback clock: the clock a game can align itself with.
    ///
    /// When this is on, libvlc tells this binding every time a video frame is
    /// displayed or a block of audio is written -- the interval depends on the source
    /// and is somewhere between 5 ms and 10 seconds -- and each report becomes
    /// [signal time_point]. [method interpolate_time_point] reads the newest report
    /// against the current system clock, which is what makes a position smooth at
    /// whatever rate the caller draws at.
    ///
    /// # Parameters
    /// - [param min_period_us] the smallest interval between reports, in
    ///   microseconds: `0` asks for all of them, and a larger value is the way to
    ///   stop a fast source from report-flooding. A negative value is refused here
    ///   (see below).
    ///
    /// # Returns
    /// `0` on success, `-1` if the period was negative or if a watcher is already
    /// registered. libvlc's header promises `-1` also for an allocation failure;
    /// unlike most of this binding's, this one really can answer `-1`.
    ///
    /// # Only one watcher, and this one is per player
    /// libvlc allows a single watcher at a time and its own second call fails, with a
    /// message in its log. This binding answers the same way without asking, and
    /// [method is_watching_time] says which state the player is in. There is no
    /// per-handler registration: [signal time_point] goes to every connected handler,
    /// and watching is one flag on the player.
    ///
    /// # A negative period would abort the process
    /// libvlc checks the period with an assertion and nothing else, and this runtime
    /// is built with assertions on: a negative value would take the process down
    /// rather than be clamped or ignored. This is the same situation as
    /// [method set_spu_text_scale]'s range, and it is handled the same way -- refused
    /// here, with the current state left alone.
    ///
    /// # Note
    /// - Watching is not per media: it can be turned on before playback and it stays
    ///   on across [method stop_async] and a changed [member media]. What it reports
    ///   while nothing plays is nothing, because libvlc has no output to report from.
    /// - [method unwatch_time] is what turns it off, and the player turns it off
    ///   itself when it is freed.
    #[func]
    fn watch_time(&mut self, min_period_us: i64) -> i32 {
        if min_period_us < 0 {
            godot_error!(
                "godot-vlc: watch_time({min_period_us}) is negative; libvlc asserts that it is not and this runtime has assertions on, so the call was not made"
            );
            return -1;
        }
        if self.time_watch.watching() {
            godot_error!(
                "godot-vlc: watch_time was called while already watching; libvlc allows one watcher at a time and the one already registered is still on"
            );
            return -1;
        }
        let status = self.time_watch.register(self.player_ptr, min_period_us);
        if status != 0 {
            godot_error!(
                "godot-vlc: libvlc refused to watch the playback clock: {}",
                crate::vlc_instance::last_error()
            );
        }
        status
    }

    /// Stops watching the playback clock. Nothing is emitted after this.
    ///
    /// # Note
    /// - Asking for this when no watcher is registered writes an error and does
    ///   nothing. libvlc's own `unwatch` assumes a watcher exists -- its check is an
    ///   assertion that a release build drops, after which it walks a null pointer --
    ///   so this binding tracks the state itself and never makes that call blind.
    /// - It is safe to call from a signal handler and from `_exit_tree`; the watcher
    ///   is also removed automatically when the player is freed.
    #[func]
    fn unwatch_time(&mut self) {
        if !self.time_watch.unregister(self.player_ptr) {
            godot_error!(
                "godot-vlc: unwatch_time was called with no watcher registered; nothing was called, because libvlc would crash on that"
            );
        }
    }

    /// Whether a watcher is registered: what [method watch_time] last did.
    ///
    /// # Returns
    /// `true` between a [method watch_time] that answered `0` and the
    /// [method unwatch_time] that follows it.
    #[func]
    fn is_watching_time(&self) -> bool {
        self.time_watch.watching()
    }

    /// The newest time point, read against the current system clock.
    ///
    /// This is the smooth playhead: it takes the last point libvlc reported and
    /// advances it by however long ago that was, at the rate playback is running at,
    /// so it can be read every frame no matter how often the reports arrive.
    ///
    /// # Returns
    /// a dictionary with these keys:
    /// - `ts_us`: int, the interpolated media time in **microseconds**, or `-1` when
    ///   there is nothing to interpolate from
    /// - `position`: float, `0.0`-`1.0`, or `-1.0` for the same reason
    ///
    /// # Note
    /// - `-1` has two meanings and they are the same answer, as with
    ///   [method get_audio_delay_us]: nothing has been reported yet because
    ///   [method watch_time] was never called or no output has run, and libvlc
    ///   answering that the interpolated time would be negative -- which is what
    ///   happens while the input is buffering. Neither is an error to report, and
    ///   both leave both keys at `-1`.
    /// - When libvlc answers that way, the time it hands back is **its own
    ///   uninitialised stack**: its core returns before writing either out-parameter,
    ///   and then its wrapper writes the time from a local the core never filled
    ///   (measured: two different six-figure numbers in two runs). Nothing of that
    ///   reaches a caller here -- the two `-1`s are this binding's sentinel, not a
    ///   reading. The call itself answers `VLC_EGENERIC` on that path rather than the
    ///   `-1` its header promises, which is the same substitution [method play] and
    ///   [method stop_async] make.
    /// - The clock is libvlc's own ([method VLCInstance.get_clock_us]), read here
    ///   rather than taken from the caller. Godot's own clocks are on a different
    ///   origin, so a system date from one of them would interpolate to a wrong time
    ///   without saying so.
    /// - A point whose clock was paused (see [signal time_point]) has nothing to
    ///   interpolate: the value is returned as it was reported.
    #[func]
    fn interpolate_time_point(&self) -> VarDictionary {
        let mut dict = VarDictionary::new();
        let Some(point) = self.time_watch.latest() else {
            dict.set("ts_us", -1i64);
            dict.set("position", -1.0f64);
            return dict;
        };
        let mut ts_us: i64 = -1;
        let mut position: f64 = -1.0;
        let status = unsafe {
            libvlc_media_player_time_point_interpolate(
                &point,
                libvlc_clock(),
                &mut ts_us,
                &mut position,
            )
        };
        if status != 0 {
            ts_us = -1;
            position = -1.0;
        }
        dict.set("ts_us", ts_us);
        dict.set("position", position);
        dict
    }

    /// Set movie title.
    ///
    /// # Parameters
    /// - [param title] title number to play
    #[func]
    fn set_title(&mut self, title: i32) {
        unsafe { libvlc_media_player_set_title(self.player_ptr, title) }
    }

    /// Set if, and how, the video title will be shown when media is played.
    ///
    /// # Parameters
    /// - [param position] position at which to display the title ([constant POSITION_CENTER], [constant POSITION_TOP],...), or [constant POSITION_DISABLE] to prevent the title from being displayed
    /// - [param timeout] title display timeout in milliseconds (ignored if [constant POSITION_DISABLE])
    #[func]
    fn set_video_title_display(&mut self, position: i32, timeout: u32) {
        unsafe { libvlc_media_player_set_video_title_display(self.player_ptr, position, timeout) }
    }

    /// Stop asynchronously.
    ///
    /// # Note
    /// This function is asynchronous. In case of success, the user should wait for the [signal stopped] signal to know when the stop is finished.
    ///
    /// # Returns
    /// `0` when the player is being stopped, or `VLC_EGENERIC` (`-2147483648`)
    /// when there was nothing to stop -- asking twice, or asking after the media
    /// ended. The `-1` that libvlc's own header documents here is never returned:
    /// what libvlc returns is what this returns, measured against the pinned
    /// runtime.
    ///
    /// # Warning
    /// - A no-op is not an error worth reporting to the user: it is what a
    ///   "stop" button pressed twice answers. [signal stopped] has already been
    ///   emitted in that case, which is the signal that says playback is over.
    /// - Stopping ends the input, and the A to B loop goes with it: see
    ///   [method set_abloop_time].
    #[func]
    fn stop_async(&mut self) -> i32 {
        unsafe { libvlc_media_player_stop_async(self.player_ptr) }
    }

    /// Unselect all tracks for a given type.
    ///
    /// # Parameters
    /// - [param track_type] type to unselect
    #[func]
    fn unselect_track_type(&mut self, track_type: i32) {
        unsafe { libvlc_media_player_unselect_track_type(self.player_ptr, track_type) }
    }

    /// Add a subtitle to the playback that is running now.\
    /// This is `libvlc_media_player_add_slave`: the subtitle is loaded into the input that exists, and its track appears as soon as the subtitle demux has opened it -- there is no need to wait for the first line to be shown.
    ///
    /// # When it works
    /// It needs an input. Before a [member media] has been assigned -- which is what creates the input, not [method play] -- there is nothing to attach a subtitle to, and libvlc returns `VLC_EGENERIC` (`INT_MIN`) without doing anything. Use [method VLCMedia.add_subtitle] before the media is assigned; that is the other half of this, and it is the only one that works there.
    ///
    /// # Nothing removes a subtitle again
    /// LibVLC has no call that unloads one: an empty track selection only unselects, and the track stays. Attaching another subtitle is the way to change what is shown, and ending the input -- [method stop_async], or a different [member media] -- is the only way to be rid of it. [method unselect_track_type] with [constant VLCTrack.TYPE_TEXT] hides one without unloading it, and so does [method select_tracks] with an empty array or [method select_tracks_by_ids] with an empty string -- all three are the same unselect.
    ///
    /// # Parameters
    /// - [param subtitle] the subtitle to attach.
    /// - [param select] whether the subtitle's track is the one to show. LibVLC does
    ///   not select subtitle tracks on its own, so `false` means "loaded, but not
    ///   shown"; `true` selects it unless a subtitle that is already showing was not
    ///   forced either, in which case the one being watched wins.
    ///
    /// # Returns
    /// `0` when the request was queued -- which is not the same as "the subtitle
    /// loaded" -- `INT_MIN` when there is no input, and a negative `errno` when the
    /// request could not be allocated. The header documents `-1 on error`; the
    /// implementation never returns `-1`.
    #[func]
    fn add_subtitle(&mut self, subtitle: Gd<VlcSubtitle>, select: bool) -> i32 {
        self.add_slave(
            libvlc_media_slave_type_t_libvlc_media_slave_type_subtitle as i32,
            subtitle.bind().get_mrl(),
            select,
        )
    }

    /// Add an external source to the input that is running now.
    ///
    /// This is `libvlc_media_player_add_slave`, and it takes a media resource locator:
    /// `data:;base64,...` for bytes the caller holds, `file:///...` for a file on the
    /// host, `http(s)://...` for one on a server. For a subtitle prefer
    /// [method add_subtitle], which is this with the subtitle type and a resource;
    /// this entry point is for the generic type, and for callers who have an MRL.
    ///
    /// A subtitle and a generic source differ in how LibVLC reads them: the subtitle
    /// type forces the `subtitle` demux, whatever the file is called, while a generic
    /// source is probed like any other media and can contribute tracks of any kind.
    ///
    /// Everything else -- needing an input, returning `INT_MIN` without one, and
    /// nothing being able to remove it again -- is [method add_subtitle]'s, which
    /// documents it.
    ///
    /// # Parameters
    /// - [param slave_type] [constant VLCMedia.SLAVE_TYPE_SUBTITLE] or
    ///   [constant VLCMedia.SLAVE_TYPE_GENERIC].
    /// - [param uri] the source's media resource locator.
    /// - [param select] whether its first track is selected; see [method add_subtitle].
    ///
    /// # Returns
    /// `0` when the request was queued, `INT_MIN` when there is no input, `-errno`
    /// when it could not be allocated.
    #[func]
    fn add_slave(&mut self, slave_type: i32, uri: GString, select: bool) -> i32 {
        let uri = cstring_from_gstring(uri);
        unsafe {
            libvlc_media_player_add_slave(
                self.player_ptr,
                slave_type as libvlc_media_slave_type_t,
                uri.as_ptr(),
                select,
            )
        }
    }

    /// Delay the display of subtitles, in microseconds.
    ///
    /// Positive values show subtitles later and negative ones earlier, which is the
    /// knob for "the subtitles are ahead of the sound".
    ///
    /// # Lifetime: the input's, not the player's
    /// The delay lives on the input. Before one exists -- before a [member media] is
    /// assigned -- the call is accepted and thrown away: libvlc returns `0` either
    /// way and there is nothing to pass on that would distinguish the two, so a delay
    /// set too early is not an error, it is simply not there. A changed media resets
    /// it to zero and [method stop_async] takes it away with the input, so it has to
    /// be set again for every playback. This is the opposite of
    /// [method set_spu_text_scale], which belongs to the player and survives both.
    ///
    /// # Parameters
    /// - [param delay_us] the delay in microseconds: `250000` is a quarter of a
    ///   second later, `-250000` a quarter of a second earlier.
    ///
    /// # Returns
    /// `0`. The header promises `-1 on error`; the implementation returns `0` on
    /// every path, including the one where there is no input and nothing happened.
    #[func]
    fn set_spu_delay_us(&mut self, delay_us: i64) -> i32 {
        unsafe { libvlc_video_set_spu_delay(self.player_ptr, delay_us) }
    }

    /// The subtitle delay, in microseconds.
    ///
    /// # Returns
    /// the delay in microseconds, or `0` for a player with no input or one that has
    /// not been given a delay.
    #[func]
    fn get_spu_delay_us(&self) -> i64 {
        unsafe { libvlc_video_get_spu_delay(self.player_ptr) }
    }

    /// Scale the size of text subtitles.
    ///
    /// `1.0` is the size the subtitle itself asks for, `2.0` twice that. Unlike
    /// [method set_spu_delay_us] this belongs to the **player**: it can be set before
    /// playback, and it outlives [method stop_async] and a changed [member media].
    ///
    /// # What it does not scale
    /// Only the text renderer reads it. ASS/SSA subtitles are drawn by libass, whose
    /// font scale is hard-coded to 1.0 in this libvlc, and bitmap subtitles (VobSub,
    /// DVD) are pictures; neither grows or shrinks with this setting.
    ///
    /// # Out-of-range values are refused
    /// LibVLC's own setter checks the range with an assertion and nothing else, and
    /// this runtime is built with assertions on: a value outside `0.1`-`5.0` would
    /// abort the process rather than be clamped or ignored. The value is therefore
    /// checked here, and a rejected one leaves the current scale alone and writes an
    /// error to the log instead. [method get_spu_text_scale] reads back what is in
    /// force.
    ///
    /// # Parameters
    /// - [param scale] the factor to scale by, `0.1`-`5.0`.
    #[func]
    fn set_spu_text_scale(&mut self, scale: f64) {
        const MIN: f64 = 0.1;
        const MAX: f64 = 5.0;
        if !(MIN..=MAX).contains(&scale) {
            godot_error!(
                "godot-vlc: set_spu_text_scale({scale}) is outside the {MIN}-{MAX} libvlc accepts; the scale was left unchanged"
            );
            return;
        }
        unsafe { libvlc_video_set_spu_text_scale(self.player_ptr, scale as f32) }
    }

    /// The subtitle text scale.
    ///
    /// # Returns
    /// the factor, `1.0` until something changes it.
    #[func]
    fn get_spu_text_scale(&self) -> f64 {
        unsafe { libvlc_video_get_spu_text_scale(self.player_ptr) as f64 }
    }

    /// Delay the sound, in microseconds.
    ///
    /// Positive values shift the audio track later and negative ones earlier -- the
    /// same direction as [method set_spu_delay_us] -- which is the knob for "the
    /// sound arrives before the picture" and for the opposite complaint.
    ///
    /// # Lifetime: the input's, not the player's
    /// Exactly like [method set_spu_delay_us]. Before an input exists the call is
    /// accepted and thrown away, a changed [member media] re-seeds the value, and
    /// [method stop_async] takes it away with the input. It is **not** zero on the
    /// next playback if the input option `audio-desync` is set: that option seeds
    /// this same value for every input.
    ///
    /// # Milliseconds in, microseconds out
    /// The input option that seeds this same value, `audio-desync`, is in
    /// **milliseconds** -- libvlc's own log line calls it "Audio desynchronization
    /// compensation" and its help text says "The delay must be given in
    /// milliseconds". This method and [method get_audio_delay_us] are in
    /// **microseconds**, a thousand times finer. `--audio-desync=250` is what
    /// [method get_audio_delay_us] reports as `250000`.
    ///
    /// # No audio output is needed
    /// Unlike [method set_spu_text_scale], and unlike every other `libvlc_audio_*`
    /// setting, this one never asks for an audio output: the value is kept on the
    /// input and handed to the decoders. It therefore reads back on a player built
    /// with `--no-audio` -- what cannot happen without an output is an audible
    /// effect, not the value.
    ///
    /// # Parameters
    /// - [param delay_us] the delay in microseconds: `250000` is a quarter of a
    ///   second later, `-250000` a quarter of a second earlier.
    ///
    /// # Returns
    /// `0`. The header promises `-1 on error`, and this one cannot fail at all:
    /// the function it calls returns `void` inside libvlc, so there is no failure
    /// for the `-1` to describe.
    #[func]
    fn set_audio_delay_us(&mut self, delay_us: i64) -> i32 {
        unsafe { libvlc_audio_set_delay(self.player_ptr, delay_us) }
    }

    /// The audio delay, in microseconds.
    ///
    /// # Returns
    /// the delay in microseconds, or `0` for a player with no input or one whose
    /// delay is zero. Those two are the same answer and there is no third one to
    /// tell them apart: `0` means "no delay" whether or not there is an input to
    /// have one.
    ///
    /// # There is no signal for this
    /// libvlc has no event for a delay change -- its player core has one, and
    /// nothing in the libvlc layer listens to it -- so polling this is the only way
    /// to see a change, including the one the `audio-desync` option makes.
    #[func]
    fn get_audio_delay_us(&self) -> i64 {
        unsafe { libvlc_audio_get_delay(self.player_ptr) }
    }
}

impl VlcMediaPlayer {
    /// The libvlc player behind this node.
    ///
    /// Used by `list_player.rs`, which hands it to
    /// `libvlc_media_list_player_set_media_player`: that call takes a pointer, not an
    /// object, and this is the only place one leaves this class.
    fn player_ptr(&self) -> *mut libvlc_media_player_t {
        self.player_ptr
    }
}

#[allow(clippy::unnecessary_cast)]
impl VlcMediaPlayer {
    /// Emits the watcher's signals for everything recorded since the last frame.
    ///
    /// Called from `on_notification`, on the main thread.
    fn emit_watched_time(&mut self) {
        while let Some(watch) = self.time_watch.pop() {
            match watch {
                time_watch::ParkedWatch::Point(point) => {
                    self.signals().time_point().emit(
                        point.ts_us,
                        point.position,
                        point.rate,
                        point.length_us,
                        point.system_date_us,
                    );
                }
                time_watch::ParkedWatch::Paused(system_date_us) => {
                    self.signals().time_point_paused().emit(system_date_us);
                }
                time_watch::ParkedWatch::Seek(point) => {
                    self.signals().time_point_seek().emit(
                        point.ts_us,
                        point.position,
                        point.rate,
                        point.length_us,
                        point.system_date_us,
                    );
                }
                time_watch::ParkedWatch::SeekFinished => {
                    self.signals().time_point_seek_finished().emit();
                }
            }
        }
    }
    fn update_media(&self) {
        if let Some(media_ptr) = self.get_media_ptr() {
            unsafe {
                libvlc_media_player_set_media(self.player_ptr, media_ptr);
            }
        }
    }

    fn update_stretch_mode(&mut self) {
        self.texture_rect.set_stretch_mode(match self.stretch_mode {
            StretchMode::Scale => TextureRectStretchMode::SCALE,
            StretchMode::Tile => TextureRectStretchMode::TILE,
            StretchMode::Keep => TextureRectStretchMode::KEEP,
            StretchMode::KeepCenterd => TextureRectStretchMode::KEEP_CENTERED,
            StretchMode::KeepAspect => TextureRectStretchMode::KEEP_ASPECT,
            StretchMode::KeepAspectCenterd => TextureRectStretchMode::KEEP_ASPECT_CENTERED,
            StretchMode::KeepAspectCovered => TextureRectStretchMode::KEEP_ASPECT_COVERED,
        });
    }

    fn update_volume_db(&mut self) {
        self.audio_player.set_volume_db(self.volume_db);
    }

    fn update_mix_target(&mut self) {
        self.audio_player.set_mix_target(match self.mix_target {
            MixTarget::Stereo => godot::classes::audio_stream_player::MixTarget::STEREO,
            MixTarget::Surround => godot::classes::audio_stream_player::MixTarget::SURROUND,
            MixTarget::Center => godot::classes::audio_stream_player::MixTarget::CENTER,
        })
    }

    fn update_bus(&mut self) {
        self.audio_player.set_bus(&self.bus);
    }

    fn get_media_ptr(&self) -> Option<*mut libvlc_media_t> {
        Some(self.media.as_ref()?.bind().media_ptr)
    }

    /// Reads the A to B loop out of libvlc, following the rules its own
    /// documentation states but its implementation does not enforce.
    ///
    /// `libvlc_media_player_get_abloop` asks the core for four values and then
    /// copies all four into the caller's pointers, whether or not they mean
    /// anything: with no loop set, the core leaves them alone, so the time
    /// outputs end up holding two uninitialised locals of libvlc's stack frame.
    /// What is meaningful is decided by the status, so this keeps only what the
    /// status covers -- from [ABLOOP_A] up for the A outputs, [ABLOOP_B] only for
    /// the B outputs -- and answers `-1`/`-1.0` for the rest.
    fn ab_loop(&self) -> AbLoop {
        let mut a_time: i64 = -1;
        let mut a_pos: f64 = -1.0;
        let mut b_time: i64 = -1;
        let mut b_pos: f64 = -1.0;
        let status = unsafe {
            libvlc_media_player_get_abloop(
                self.player_ptr,
                &mut a_time,
                &mut a_pos,
                &mut b_time,
                &mut b_pos,
            )
        };
        let has_a = status >= libvlc_abloop_t_libvlc_abloop_a;
        let has_b = status >= libvlc_abloop_t_libvlc_abloop_b;
        AbLoop {
            // `as i32`: `libvlc_abloop_t` is `c_int` on Windows and `u32` on the
            // Linux and Android targets, so the cast is required by some of them
            // and redundant on the others.
            status: status as i32,
            a_time: if has_a { a_time } else { -1 },
            a_pos: if has_a { a_pos } else { -1.0 },
            b_time: if has_b { b_time } else { -1 },
            b_pos: if has_b { b_pos } else { -1.0 },
        }
    }

    /// Bring up the GPU output backend. `true` if it activated; `false`
    /// means the caller should register the software callbacks instead.
    /// Failures are logged via `godot_error!`; no panics.
    #[cfg(all(feature = "gpu", windows))]
    pub(crate) fn try_init_gpu_backend(&mut self) -> bool {
        if !self.force_hardware {
            return false;
        }
        use std::sync::Arc;
        let (_adapter, d3d11) = match gpu_d3d11::adapter::create_d3d11_device_for_godot() {
            Ok(v) => v,
            Err(e) => {
                godot_error!("godot-vlc: GPU init failed (adapter/D3D11 device): {e}");
                return false;
            }
        };
        let mailbox = Arc::new(gpu_d3d11::event_queue::EventMailbox::new());
        let backend = match gpu_d3d11::output_callbacks::Backend::new(
            d3d11.device,
            d3d11.context,
            mailbox.clone(),
        ) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                godot_error!("godot-vlc: GPU init failed (Backend::new): {e}");
                return false;
            }
        };
        let importer = match gpu_d3d11::importer::ImporterTask::create(backend.clone()) {
            Ok(t) => t,
            Err(e) => {
                godot_error!("godot-vlc: GPU init failed (ImporterTask::create): {e}");
                return false;
            }
        };
        // Swap the texture_rect's bound texture to the GPU-path Texture2Drd.
        {
            let drd = importer
                .texture_2drd
                .lock()
                .expect("texture_2drd poisoned")
                .clone();
            self.texture_rect.set_texture(&drd.upcast::<Texture2D>());
        }
        // Per-frame import + copy: frame_pre_draw → call_on_render_thread.
        let frame_task = importer.clone();
        let frame_callable = Callable::from_sync_fn("godot_vlc_per_frame", move |_args| {
            let task = frame_task.clone();
            let render_callable =
                Callable::from_sync_fn("godot_vlc_per_frame_render", move |_args| {
                    gpu_d3d11::importer::run_frame(&task);
                    Variant::nil()
                });
            RenderingServer::singleton().call_on_render_thread(&render_callable);
            Variant::nil()
        });
        // Untyped Object::connect: godot-rust 0.3.5's typed-signal accessor
        // takes a closure with no disconnect path; we need a Callable handle
        // to disconnect on Drop.
        RenderingServer::singleton().connect(&StringName::from("frame_pre_draw"), &frame_callable);
        if let Err(e) = gpu_d3d11::output_callbacks::register(self.player_ptr, backend.clone()) {
            godot_error!("godot-vlc: GPU init failed ({e})");
            RenderingServer::singleton()
                .disconnect(&StringName::from("frame_pre_draw"), &frame_callable);
            return false;
        }
        self.gpu_backend = Some(backend);
        self.gpu_mailbox = Some(mailbox);
        self.gpu_importer = Some(importer);
        self.gpu_frame_callable = Some(frame_callable);
        godot_print!("godot-vlc: GPU backend active (D3D11→D3D12 GPU-copy)");
        true
    }

    /// Stub for non-GPU builds. Always returns false so the software path
    /// runs.
    #[cfg(not(all(feature = "gpu", windows)))]
    pub(crate) fn try_init_gpu_backend(&mut self) -> bool {
        false
    }
}
