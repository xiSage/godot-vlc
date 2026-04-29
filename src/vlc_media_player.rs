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

mod internal_audio_stream;
pub mod internal_audio_stream_playback;
mod software_video;

#[cfg(all(feature = "gpu", windows))]
mod gpu_d3d11;

use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    ptr::slice_from_raw_parts,
    sync::mpsc,
};

use crate::{
    vlc::*,
    vlc_instance::{self},
    vlc_media::VlcMedia,
    vlc_media_player::internal_audio_stream::InternalAudioStream,
    vlc_track::VlcTrack,
    vlc_track_list::VlcTrackList,
};
use godot::{
    classes::{
        control::{LayoutPreset, LayoutPresetMode},
        native::AudioFrame,
        node::InternalMode,
        notify::ControlNotification,
        texture_rect::{ExpandMode, StretchMode as TextureRectStretchMode},
        AudioServer, AudioStream, AudioStreamPlayer, Control, IControl, Image, ImageTexture,
        RenderingServer, Texture2D, TextureRect,
    },
    obj::NewAlloc,
    prelude::*,
};
use ringbuf::{
    traits::{Producer, Split},
    HeapProd, HeapRb,
};

#[derive(GodotConvert, Var, Export)]
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

#[derive(GodotConvert, Var, Export)]
#[godot(via=i64)]
pub enum MixTarget {
    Stereo,
    Surround,
    Center,
}

/// A control used for video playback.\
/// This control provides a simple way to play video files using the VLC library. It supports most common video formats, including MP4, MKV, AVI, etc.
#[derive(GodotClass)]
#[class(base=Control, rename=VLCMediaPlayer)]
struct VlcMediaPlayer {
    base: Base<Control>,
    #[export]
    #[var(get, set=set_media)]
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
    #[var(get, set=set_stretch_mode)]
    stretch_mode: StretchMode,
    #[export(range = (-80.0, 24.0, suffix="db"))]
    #[var(get, set=set_volume_db)]
    volume_db: f32,
    #[export]
    #[var(get, set=set_mix_target)]
    mix_target: MixTarget,
    #[export]
    #[var(get, set=set_bus)]
    bus: StringName,
    player_ptr: *mut libvlc_media_player_t,
    self_gd: Option<Box<Gd<Self>>>,
    texture: Gd<ImageTexture>,
    texture_rect: Gd<TextureRect>,
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
            self_gd: None,
            texture,
            texture_rect: texture_rect.clone(),
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
            gpu_frame_callable: None,
        }
    }

    fn on_notification(&mut self, what: ControlNotification) {
        if what == ControlNotification::INTERNAL_PROCESS {
            if let Ok(data) = self.video_rx.try_recv() {
                if data.1.is_instance_valid() && !data.1.is_empty() && data.1.get_data_size() > 0 {
                    if data.0 {
                        self.texture.set_image(&data.1);
                    } else {
                        self.texture.update(&data.1);
                    }
                    self.signals().video_frame().emit();
                }
            }
        } else if what == ControlNotification::READY {
            self.self_gd = Some(Box::new(self.to_gd()));
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
        // Disconnect the per-frame callable BEFORE releasing the player —
        // an in-flight frame_pre_draw must not run against a half-torn
        // ImporterTask.
        #[cfg(all(feature = "gpu", windows))]
        if let Some(c) = self.gpu_frame_callable.take() {
            RenderingServer::singleton()
                .disconnect(&StringName::from("frame_pre_draw"), &c);
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

    #[signal]
    fn openning();
    #[signal]
    fn buffering();
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
    #[signal]
    fn video_frame();

    #[func]
    pub fn set_media(&mut self, media: Option<Gd<VlcMedia>>) {
        self.media = media;
        self.update_media();
    }

    #[func]
    fn get_texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
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

    /// Number of per-frame `copy_and_sync` invocations completed by the
    /// importer. 0 if the GPU backend isn't active.
    #[cfg(all(feature = "gpu", windows))]
    #[func]
    fn _debug_frames_copied(&self) -> i64 {
        match &self.gpu_importer {
            None => 0,
            Some(t) => t
                .frames_copied
                .load(std::sync::atomic::Ordering::SeqCst) as i64,
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
            b.update_output_calls.load(std::sync::atomic::Ordering::SeqCst) as i32,
            b.swap_calls.load(std::sync::atomic::Ordering::SeqCst) as i32,
            b.make_current_calls.load(std::sync::atomic::Ordering::SeqCst) as i32,
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
        for chunk in bytes.as_slice().chunks_exact(4) {
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
    fn _debug_get_adapter_luids(&self) -> Dictionary {
        use gpu_d3d11::adapter::{dxgi_adapter_luid_for, godot_d3d12_luid};
        let mut dict = Dictionary::new();
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

    /// Can this media player be paused?
    ///
    /// # Return values
    /// - `true` media player can be paused
    /// - `false` media player cannot be paused
    #[func]
    fn can_pause(&self) -> bool {
        unsafe { libvlc_media_player_can_pause(self.player_ptr) }
    }

    /// Get movie chapter.
    ///
    /// # Returns
    /// chapter number currently playing, or -1 if there is no media.
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
    /// - [param title] title
    ///
    /// # Returns
    /// number of chapters in title, or -1.
    #[func]
    fn get_chapter_count_for_title(&self, title: i32) -> i32 {
        unsafe { libvlc_media_player_get_chapter_count_for_title(self.player_ptr, title) }
    }

    /// Get the current movie length (in ms).
    ///
    /// # Returns
    /// the movie length (in ms), or -1 if there is no media.
    #[func]
    fn get_length(&self) -> i64 {
        unsafe { libvlc_media_player_get_length(self.player_ptr) }
    }

    /// Get movie position as percentage between 0.0 and 1.0.
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
    /// # Returns
    /// the movie time (in ms), or -1 if there is no media.
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

    /// Get the track list for one type.\
    /// The track list can be used to get track information and to select specific tracks.
    ///
    /// # Note
    /// You need to call [method VLCMedia.parse_request] or play the media at least once before calling this function. Not doing this will result in an empty list.\
    /// This track list is a snapshot of the current tracks when this function is called. If a track is updated after this call, the user will need to call this function again to get the updated track.
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
        VlcTrackList::from_ptr(ptr)
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
    /// # Return values
    /// - `true` media player can seek
    /// - `false` media player cannot seek
    #[func]
    fn is_seekable(&self) -> bool {
        unsafe { libvlc_media_player_is_seekable(self.player_ptr) }
    }

    // /// Jump the movie time (in ms).\
    // /// This will trigger a precise and relative seek (from the current time). This has no effect if no media is being played. Not all formats and protocols support this.
    // ///
    // /// # Parameters
    // /// - [param time] the movie time (in ms).
    // ///
    // /// # Returns
    // /// 0 on success, -1 on error.
    // #[func()]
    // fn jump_time(&mut self, time: i64) -> i32 {
    //     unsafe { libvlc_media_player_jump_time(self.player_ptr, time) }
    // }

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
    /// 0 if playback started (and was already started), or -1 on error.
    #[func]
    fn play(&mut self) -> i32 {
        unsafe { libvlc_media_player_play(self.player_ptr) }
    }

    /// Set previous chapter (if applicable)
    #[func]
    fn previous_chapter(&mut self) {
        unsafe { libvlc_media_player_previous_chapter(self.player_ptr) }
    }

    /// Select a track.\
    /// This will unselected the current track.
    ///
    /// # Warning
    /// Only use a libvlc_media_track_t retrieved with libvlc_media_player_get_tracklist
    ///
    /// # Parameters
    /// - [param track] track to select, can't be NULL
    #[func]
    fn select_track(&mut self, track: Gd<VlcTrack>) {
        unsafe { libvlc_media_player_select_track(self.player_ptr, track.bind().ptr) }
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
    /// 0 if the player is being stopped, -1 otherwise (no-op)
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

    #[func]
    fn set_stretch_mode(&mut self, stretch_mode: i64) {
        self.stretch_mode = match stretch_mode {
            0 => StretchMode::Scale,
            1 => StretchMode::Tile,
            2 => StretchMode::Keep,
            3 => StretchMode::KeepCenterd,
            4 => StretchMode::KeepAspect,
            5 => StretchMode::KeepAspectCenterd,
            6 => StretchMode::KeepAspectCovered,
            _ => StretchMode::KeepAspectCenterd,
        };
        self.update_stretch_mode();
    }

    #[func]
    fn set_volume_db(&mut self, volume_db: f32) {
        self.volume_db = volume_db;
        self.update_volume_db();
    }

    #[func]
    fn set_mix_target(&mut self, mix_target: i64) {
        self.mix_target = match mix_target {
            0 => MixTarget::Stereo,
            1 => MixTarget::Surround,
            2 => MixTarget::Center,
            _ => MixTarget::Stereo,
        };
        self.update_mix_target();
    }

    #[func]
    fn set_bus(&mut self, bus: StringName) {
        self.bus = bus;
        self.update_bus();
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
            MixTarget::Surround => godot::classes::audio_stream_player::MixTarget::STEREO,
            MixTarget::Center => godot::classes::audio_stream_player::MixTarget::STEREO,
        })
    }

    fn update_bus(&mut self) {
        self.audio_player.set_bus(&self.bus);
    }

    fn get_media_ptr(&self) -> Option<*mut libvlc_media_t> {
        Some(self.media.as_ref()?.bind().media_ptr)
    }

    /// Bring up the GPU output backend. `true` if it activated; `false`
    /// means the caller should register the software callbacks instead.
    /// Failures are logged via `godot_error!`; no panics.
    #[cfg(all(feature = "gpu", windows))]
    fn try_init_gpu_backend(&mut self) -> bool {
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
                    Ok(Variant::nil())
                });
            RenderingServer::singleton().call_on_render_thread(&render_callable);
            Ok(Variant::nil())
        });
        // Untyped Object::connect: godot-rust 0.3.5's typed-signal accessor
        // takes a closure with no disconnect path; we need a Callable handle
        // to disconnect on Drop.
        RenderingServer::singleton()
            .connect(&StringName::from("frame_pre_draw"), &frame_callable);
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
    fn try_init_gpu_backend(&mut self) -> bool {
        false
    }

    fn register_player_callbacks(&mut self) {
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
                Some(audio_play_callback),
                Some(audio_pause_callback),
                Some(audio_resume_callback),
                Some(audio_flush_callback),
                Some(audio_drain_callback),
                self.audio_prod.as_mut() as *mut _ as *mut c_void,
            );
            libvlc_audio_set_format_callbacks(
                self.player_ptr,
                Some(audio_setup_callback),
                Some(audio_cleanup_callback),
            );

            fn get_player(ptr: *mut c_void) -> Gd<VlcMediaPlayer> {
                unsafe { (ptr as *mut Gd<VlcMediaPlayer>).as_mut().unwrap().clone() }
            }
            let event_manager = libvlc_media_player_event_manager(self.player_ptr);

            unsafe extern "C" fn openning_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data)
                    .call_deferred("emit_signal", &[StringName::from(c"openning").to_variant()]);
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaPlayerOpening as libvlc_event_type_t,
                Some(openning_callback),
                self_ptr as *mut c_void,
            );

            unsafe extern "C" fn buffering_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                get_player(user_data).call_deferred(
                    "emit_signal",
                    &[StringName::from(c"buffering").to_variant()],
                );
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
                    .call_deferred("emit_signal", &[StringName::from(c"playing").to_variant()]);
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
                    .call_deferred("emit_signal", &[StringName::from(c"paused").to_variant()]);
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
                    .call_deferred("emit_signal", &[StringName::from(c"stopped").to_variant()]);
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
                    .call_deferred("emit_signal", &[StringName::from(c"forward").to_variant()]);
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
                    .call_deferred("emit_signal", &[StringName::from(c"backward").to_variant()]);
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
                    .call_deferred("emit_signal", &[StringName::from(c"stopping").to_variant()]);
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

unsafe extern "C" fn audio_play_callback(
    data: *mut c_void,
    samples: *const c_void,
    count: c_uint,
    _pts: i64,
) {
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

unsafe extern "C" fn audio_pause_callback(data: *mut c_void, _pts: i64) {
    let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
        .as_mut()
        .unwrap();
    player.set_stream_paused(true);
}

unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
    let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
        .as_mut()
        .unwrap();
    player.set_stream_paused(false);
}

unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
    let (_, player) = (data as *mut (HeapProd<AudioFrame>, Gd<AudioStreamPlayer>))
        .as_mut()
        .unwrap();
    if player.is_instance_valid() {
        player.call_thread_safe("stop", &[]);
        if let Some(stream) = player.get_stream() {
            if let Ok(mut internal_stream) = stream.try_cast::<InternalAudioStream>() {
                internal_stream
                    .bind_mut()
                    .playback
                    .bind_mut()
                    .clear_buffer();
            }
        }
    }
}

unsafe extern "C" fn audio_drain_callback(_data: *mut c_void) {
    // do nothing
}

unsafe extern "C" fn audio_setup_callback(
    _opaque: *mut *mut c_void,
    format: *mut c_char,
    rate: *mut c_uint,
    channels: *mut c_uint,
) -> c_int {
    format.copy_from(c"FL32".as_ptr(), 5);
    *rate = AudioServer::singleton().get_mix_rate() as c_uint;
    *channels = 2;
    0
}

unsafe extern "C" fn audio_cleanup_callback(_opaque: *mut c_void) {
    // do nothing
}
