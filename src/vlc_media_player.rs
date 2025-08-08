use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    ptr,
    sync::mpsc,
};

use crate::{
    vlc::*,
    vlc_instance::{self},
    vlc_media::VlcMedia,
};
use godot::{
    classes::{
        control::{LayoutPreset, LayoutPresetMode},
        image,
        node::InternalMode,
        notify::ControlNotification,
        texture_rect::{ExpandMode, StretchMode as TextureRectStretchMode},
        Control, IControl, Image, ImageTexture, Texture2D, TextureRect,
    },
    prelude::*,
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
    #[export]
    #[var(get, set=set_stretch_mode)]
    stretch_mode: StretchMode,
    player_ptr: *mut libvlc_media_player_t,
    self_gd: *mut Gd<Self>,
    texture: Gd<ImageTexture>,
    texture_rect: Gd<TextureRect>,
    video_tx: *mut mpsc::Sender<(bool, Gd<Image>)>, // (is_resized, image)
    video_rx: mpsc::Receiver<(bool, Gd<Image>)>,
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
        let video_tx = Box::into_raw(Box::new(video_tx));

        Self {
            base,
            media: None,
            autoplay: false,
            stretch_mode: StretchMode::KeepAspectCenterd,
            player_ptr,
            self_gd: ptr::null_mut(),
            texture,
            texture_rect: texture_rect.clone(),
            video_tx,
            video_rx,
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
                }
            }
        } else if what == ControlNotification::READY {
            self.self_gd = Box::into_raw(Box::new(self.to_gd()));
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

            self.update_media();
            self.update_stretch_mode();

            self.base_mut().set_process_internal(true);
            if self.autoplay {
                self.play();
            }
        }
    }
}

impl Drop for VlcMediaPlayer {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_player_release(self.player_ptr);
        }
        self.texture_rect.queue_free();
        if !self.self_gd.is_null() {
            unsafe {
                drop(Box::from_raw(self.self_gd));
            }
        }
        if !self.video_tx.is_null() {
            unsafe {
                drop(Box::from_raw(self.video_tx));
            }
        }
    }
}

#[godot_api]
impl VlcMediaPlayer {
    #[constant]
    const STATE_NOTHING_SPECIAL: i32 = libvlc_state_t_libvlc_NothingSpecial;
    #[constant]
    const STATE_OPENING: i32 = libvlc_state_t_libvlc_Opening;
    #[constant]
    const STATE_BUFFERING: i32 = libvlc_state_t_libvlc_Buffering;
    #[constant]
    const STATE_PLAYING: i32 = libvlc_state_t_libvlc_Playing;
    #[constant]
    const STATE_PAUSED: i32 = libvlc_state_t_libvlc_Paused;
    #[constant]
    const STATE_STOPPED: i32 = libvlc_state_t_libvlc_Stopped;
    #[constant]
    const STATE_STOPPING: i32 = libvlc_state_t_libvlc_Stopping;
    #[constant]
    const STATE_ERROR: i32 = libvlc_state_t_libvlc_Error;

    #[constant]
    const NAVIGATE_ACTIVATE: i32 = libvlc_navigate_mode_t_libvlc_navigate_activate;
    #[constant]
    const NAVIGATE_UP: i32 = libvlc_navigate_mode_t_libvlc_navigate_up;
    #[constant]
    const NAVIGATE_DOWN: i32 = libvlc_navigate_mode_t_libvlc_navigate_down;
    #[constant]
    const NAVIGATE_LEFT: i32 = libvlc_navigate_mode_t_libvlc_navigate_left;
    #[constant]
    const NAVIGATE_RIGHT: i32 = libvlc_navigate_mode_t_libvlc_navigate_right;
    #[constant]
    const NAVIGATE_POPUP: i32 = libvlc_navigate_mode_t_libvlc_navigate_popup;

    #[constant]
    const POSITION_DISABLE: i32 = libvlc_position_t_libvlc_position_disable;
    #[constant]
    const POSITION_CENTER: i32 = libvlc_position_t_libvlc_position_center;
    #[constant]
    const POSITION_LEFT: i32 = libvlc_position_t_libvlc_position_left;
    #[constant]
    const POSITION_RIGHT: i32 = libvlc_position_t_libvlc_position_right;
    #[constant]
    const POSITION_TOP: i32 = libvlc_position_t_libvlc_position_top;
    #[constant]
    const POSITION_TOP_LEFT: i32 = libvlc_position_t_libvlc_position_top_left;
    #[constant]
    const POSITION_TOP_RIGHT: i32 = libvlc_position_t_libvlc_position_top_right;
    #[constant]
    const POSITION_BOTTOM: i32 = libvlc_position_t_libvlc_position_bottom;
    #[constant]
    const POSITION_BOTTOM_LEFT: i32 = libvlc_position_t_libvlc_position_bottom_left;
    #[constant]
    const POSITION_BOTTOM_RIGHT: i32 = libvlc_position_t_libvlc_position_bottom_right;

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

    #[func]
    pub fn set_media(&mut self, media: Option<Gd<VlcMedia>>) {
        self.media = media;
        self.update_media();
    }

    #[func]
    fn get_texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
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
        unsafe { libvlc_media_player_get_state(self.player_ptr) }
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

    /// Jump the movie time (in ms).\
    /// This will trigger a precise and relative seek (from the current time). This has no effect if no media is being played. Not all formats and protocols support this.
    ///
    /// # Parameters
    /// - [param time] the movie time (in ms).
    ///
    /// # Returns
    /// 0 on success, -1 on error.
    #[func()]
    fn jump_time(&mut self, time: i64) -> i32 {
        unsafe { libvlc_media_player_jump_time(self.player_ptr, time) }
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

    fn get_media_ptr(&self) -> Option<*mut libvlc_media_t> {
        Some(self.media.as_ref()?.bind().media_ptr)
    }

    fn register_player_callbacks(&mut self) {
        unsafe {
            libvlc_video_set_callbacks(
                self.player_ptr,
                Some(video_lock_callback),
                Some(video_unlock_callback),
                Some(video_display_callback),
                self.video_tx as *mut c_void,
            );
            libvlc_video_set_format_callbacks(
                self.player_ptr,
                Some(video_format_callback),
                Some(video_cleanup_callback),
            );
            // libvlc_audio_set_callbacks(
            //     self.player_ptr,
            //     Some(audio_play_callback),
            //     Some(audio_pause_callback),
            //     Some(audio_resume_callback),
            //     Some(audio_flush_callback),
            //     Some(audio_drain_callback),

            // );

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
                libvlc_event_e_libvlc_MediaPlayerOpening,
                Some(openning_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerBuffering,
                Some(buffering_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerPlaying,
                Some(playing_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerPaused,
                Some(paused_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerStopped,
                Some(stopped_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerForward,
                Some(forward_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerBackward,
                Some(backward_callback),
                self.self_gd as *mut c_void,
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
                libvlc_event_e_libvlc_MediaPlayerStopping,
                Some(stopping_callback),
                self.self_gd as *mut c_void,
            );
        }
    }
}

unsafe extern "C" fn video_lock_callback(
    opaque: *mut c_void,
    planes: *mut *mut c_void,
) -> *mut c_void {
    let (_tx, _img, buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    let buffer_ptr = buffer.as_mut_slice().as_mut_ptr();
    *planes = buffer_ptr as *mut c_void;
    ptr::null_mut()
}

unsafe extern "C" fn video_unlock_callback(
    opaque: *mut c_void,
    _picture: *mut c_void,
    _planes: *const *mut c_void,
) {
    let (_tx, img, buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    let width = img.get_width();
    let height = img.get_height();
    let format = img.get_format();
    img.set_data(width, height, false, format, buffer);
}

unsafe extern "C" fn video_display_callback(opaque: *mut c_void, _picture: *mut c_void) {
    let (tx, img, _buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    _ = tx.as_mut().unwrap().send((false, img.clone()));
}

unsafe extern "C" fn video_format_callback(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    let tx: *mut mpsc::Sender<(bool, Gd<Image>)> = *opaque as *mut mpsc::Sender<(bool, Gd<Image>)>;
    let img = match Image::create_empty(*width as i32, *height as i32, false, image::Format::RGB8) {
        Some(img) => img,
        None => {
            return 0;
        }
    };
    let buffer = img.get_data();
    chroma.copy_from(c"RV24".as_ptr(), 5);
    *pitches = *width * 3;
    *lines = *height;
    if tx.as_mut().unwrap().send((true, img.clone())).is_err() {
        return 0;
    }
    *opaque = Box::into_raw(Box::new((tx, img, buffer))) as *mut c_void;
    1
}
unsafe extern "C" fn video_cleanup_callback(opaque: *mut c_void) {
    let (_tx, _img, _buffer) = *Box::from_raw(
        opaque
            as *mut (
                *mut mpsc::Sender<(bool, Gd<Image>)>,
                Gd<Image>,
                PackedByteArray,
            ),
    );
}

unsafe extern "C" fn audio_play_callback(
    data: *mut c_void,
    samples: *const c_void,
    count: c_uint,
    _pts: i64,
) {
    !unimplemented!()
}

unsafe extern "C" fn audio_pause_callback(data: *mut c_void, _pts: i64) {
    !unimplemented!()
}

unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
    !unimplemented!()
}

unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
    !unimplemented!()
}

unsafe extern "C" fn audio_drain_callback(_data: *mut c_void) {
    !unimplemented!()
}

unsafe extern "C" fn audio_setup_callback(
    opaque: *mut *mut c_void,
    format: *mut c_char,
    rate: *mut c_uint,
    channels: *mut c_uint,
) -> c_int {
    !unimplemented!()
}

unsafe extern "C" fn audio_cleanup_callback(opaque: *mut c_void) {
    !unimplemented!()
}
