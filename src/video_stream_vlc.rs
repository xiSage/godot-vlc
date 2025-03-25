use crate::vlc::{self, libvlc_track_type_t_libvlc_track_audio};
use crate::vlc_instance::VLCInstance;
use core::{slice, time};
use godot::classes::image::Format;
use godot::classes::notify::ObjectNotification;
use godot::classes::{
    file_access::ModeFlags, image, Engine, IVideoStream, IVideoStreamPlayback, Image, ImageTexture,
    Texture2D, VideoStream, VideoStreamPlayback,
};
use godot::prelude::*;
use std::ffi::{c_char, c_int, c_uchar, c_uint, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::ptr;
use std::sync::Mutex;
use std::thread::sleep;

enum MediaType { File, Location }

#[derive(GodotClass)]
#[class(base=VideoStream, init)]
pub struct VideoStreamVLC {
    base: Base<VideoStream>,
    #[init(val = MediaType::File)]
    media_type: MediaType,
}

#[godot_api]
impl IVideoStream for VideoStreamVLC {
    fn instantiate_playback(&mut self) -> Option<Gd<VideoStreamPlayback>> {
        let playback = match self.media_type {
            MediaType::File => {
                let file = self.base_mut().get_file();
                VideoStreamVLCPlayback::from_file(file)
            },
            MediaType::Location => VideoStreamVLCPlayback::from_location(self.base_mut().get_file())
        };

        playback.map(|playback| playback.upcast())
    }
}

#[godot_api]
impl VideoStreamVLC {
    #[func]
    fn create_from_location(location: GString) -> Gd<Self> {
        let mut inst = Gd::from_init_fn(|base| Self {
            base,
            media_type: MediaType::Location
        });
        inst.bind_mut().base_mut().set_file(&location);
        inst
    }
}

#[derive(GodotClass)]
#[class(base=VideoStreamPlayback, no_init)]
pub struct VideoStreamVLCPlayback {
    base: Base<VideoStreamPlayback>,
    #[allow(dead_code)]
    media: Media,
    player: *mut vlc::libvlc_media_player_t,
    texture: *mut Gd<ImageTexture>,
    audio_data: *mut Mutex<AudioData>,
    playing: bool,
    paused: bool,
    audio_track: i32,
}

#[godot_api]
impl IVideoStreamPlayback for VideoStreamVLCPlayback {
    fn stop(&mut self) {
        unsafe {
            vlc::libvlc_media_player_stop_async(self.player);
        }
        loop {
            sleep(time::Duration::from_millis(10));
            unsafe {
                if vlc::libvlc_media_player_get_state(self.player)
                    == vlc::libvlc_state_t_libvlc_Stopped
                {
                    break;
                }
            }
        }
        self.playing = false;
    }
    fn play(&mut self) {
        unsafe {
            vlc::libvlc_media_player_play(self.player);
        }
        loop {
            sleep(time::Duration::from_millis(10));
            if unsafe { vlc::libvlc_media_player_is_playing(self.player) } {
                break;
            }
        }
        self.set_audio_track(self.audio_track);
        self.playing = true;
        unsafe {
            sleep(time::Duration::from_millis(100));
            vlc::libvlc_media_player_set_pause(self.player, self.paused as c_int);
        }
    }
    fn is_playing(&self) -> bool {
        self.playing
    }
    fn set_paused(&mut self, paused: bool) {
        let audio_data = unsafe { self.audio_data.as_mut().unwrap().get_mut().unwrap() };
        audio_data.buffer.clear();
        audio_data.frames = 0;
        self.paused = paused;
        if self.playing {
            unsafe {
                vlc::libvlc_media_player_set_pause(self.player, paused as c_int);
            }
        }
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn get_length(&self) -> f64 {
        unsafe { vlc::libvlc_media_player_get_length(self.player) as f64 / 1000.0 }
    }
    fn get_playback_position(&self) -> f64 {
        unsafe { vlc::libvlc_media_player_get_time(self.player) as f64 / 1000.0 }
    }
    fn seek(&mut self, time: f64) {
        unsafe {
            match vlc::libvlc_media_player_set_time(self.player, (time * 1000.0) as i64, false) {
                0 => {}
                _ => {
                    godot_warn!("LibVLC: seek failed")
                }
            }
        }
    }
    fn set_audio_track(&mut self, idx: i32) {
        unsafe {
            let tracklist = vlc::libvlc_media_player_get_tracklist(
                self.player,
                libvlc_track_type_t_libvlc_track_audio,
                false,
            );
            if tracklist.is_null() {
                return;
            }
            let count = vlc::libvlc_media_tracklist_count(tracklist) as i32;
            if count > 0 && idx >= 0 && idx < count {
                let track = vlc::libvlc_media_tracklist_at(tracklist, idx as usize);
                vlc::libvlc_media_player_select_track(self.player, track);
            }
            self.audio_track = idx;
            vlc::libvlc_media_tracklist_delete(tracklist);
        }
    }
    fn get_texture(&self) -> Option<Gd<Texture2D>> {
        let texture = unsafe { self.texture.as_ref()? };
        Some(texture.clone().upcast())
    }
    fn update(&mut self, _delta: f64) {
        self.playing = unsafe { vlc::libvlc_media_player_is_playing(self.player) };
        let audio_data = unsafe { self.audio_data.as_mut().unwrap().get_mut().unwrap() };
        if self.playing && !audio_data.paused && audio_data.frames > 0 {
            let count = audio_data.frames as usize * audio_data.channels as usize;
            let mut array = PackedFloat32Array::from([0.0; 2048]);
            let mut mixed = 0;
            while (count - mixed) > 0 {
                let len = if count - mixed > 2048 {
                    2048
                } else {
                    count - mixed
                };
                array.as_mut_slice()[..len].copy_from_slice(&audio_data.buffer[mixed..mixed + len]);
                let result = self
                    .base_mut()
                    .mix_audio_ex(len as i32 / audio_data.channels)
                    .buffer(&array)
                    .offset(0)
                    .done() as usize
                    * audio_data.channels as usize;
                if result == 0 {
                    audio_data.buffer = audio_data.buffer.drain(..mixed).collect();
                    audio_data.frames -= mixed as i32 / audio_data.channels;
                    break;
                }
                mixed += result;
            }
            audio_data.buffer.clear();
            audio_data.frames = 0;
        }
    }
    fn get_channels(&self) -> i32 {
        unsafe {
            self.audio_data
                .as_mut()
                .unwrap()
                .get_mut()
                .unwrap()
                .channels
        }
    }
    fn get_mix_rate(&self) -> i32 {
        unsafe { self.audio_data.as_mut().unwrap().get_mut().unwrap().rate }
    }
    fn on_notification(&mut self, what: ObjectNotification) {
        if what == ObjectNotification::PREDELETE {
            unsafe {
                vlc::libvlc_media_player_release(self.player);
                drop(Box::from_raw(self.texture));
                drop(Box::from_raw(self.audio_data));
            }
        }
    }
}

#[godot_api]
impl VideoStreamVLCPlayback {
    #[func]
    fn from_file(file: GString) -> Option<Gd<Self>> {
        let file = GFile::open(&file, ModeFlags::READ);
        if file.is_err() {
            godot_error!("godot-vlc: unable to open file");
            return None;
        }
        let file = file.unwrap();
        let media = Media::from_file(file);
        Self::from_media(media)
    }

    #[func]
    fn from_location(location: GString) -> Option<Gd<Self>> {
        let media = Media::from_location(location);
        Self::from_media(media)
    }

    fn from_media(media: Media) -> Option<Gd<Self>> {
        if media.get_media_ptr().is_null() {
            godot_error!("godot-vlc: unable to create media");
            return None;
        }
        let mut instance_singleton: Gd<VLCInstance> = Engine::singleton()
            .get_singleton("VLCInstance")
            .expect("VLCInstance not found")
            .cast();
        let inst = instance_singleton.bind_mut().get_vlc_instance();
        let player =
            unsafe { vlc::libvlc_media_player_new_from_media(inst, media.get_media_ptr()) };
        let texture = Box::into_raw(Box::new(ImageTexture::new_gd()));
        let audio_data = AudioData {
            buffer: vec![0f32; 0],
            frames: 0,
            rate: 0,
            channels: 2,
            paused: false,
        };
        let audio_data = Box::into_raw(Box::new(Mutex::new(audio_data)));
        unsafe {
            vlc::libvlc_video_set_callbacks(
                player,
                Some(Self::video_lock_callback),
                Some(Self::video_unlock_callback),
                None,
                texture as *mut _ as *mut c_void,
            );
            vlc::libvlc_video_set_format_callbacks(
                player,
                Some(Self::video_format_callback),
                Some(Self::video_cleanup_callback),
            );
            vlc::libvlc_audio_set_callbacks(
                player,
                Some(Self::audio_play_callback),
                Some(Self::audio_pause_callback),
                Some(Self::audio_resume_callback),
                Some(Self::audio_flush_callback),
                Some(Self::audio_drain_callback),
                audio_data as *mut _,
            );
            vlc::libvlc_audio_set_format_callbacks(
                player,
                Some(Self::audio_setup_callback),
                Some(Self::audio_cleanup_callback),
            );
        }
        unsafe {
            let result = vlc::libvlc_media_player_play(player);
            vlc::libvlc_media_player_pause(player);
            if result == 0 {
                loop {
                    sleep(time::Duration::from_millis(10));
                    if vlc::libvlc_media_player_is_playing(player) {
                        break;
                    }
                }
            }

            sleep(time::Duration::from_millis(100));
            vlc::libvlc_media_player_stop_async(player);
            vlc::libvlc_media_player_pause(player);
        }
        Some(Gd::from_init_fn(|base| Self {
            base,
            media,
            player,
            texture,
            audio_data,
            playing: false,
            paused: false,
            audio_track: 0,
        }))
    }

    unsafe extern "C" fn video_lock_callback(
        opaque: *mut c_void,
        planes: *mut *mut c_void,
    ) -> *mut c_void {
        let (_, buffer) = (opaque as *mut (*mut Gd<ImageTexture>, Vec<u8>))
            .as_mut()
            .unwrap();
        *planes = buffer.as_mut_ptr() as *mut _;
        ptr::null_mut()
    }

    unsafe extern "C" fn video_unlock_callback(
        opaque: *mut c_void,
        _picture: *mut c_void,
        _planes: *const *mut c_void,
    ) {
        let (texture, buffer) = (opaque as *mut (*mut Gd<ImageTexture>, Vec<u8>))
            .as_mut()
            .unwrap();
        let texture = texture.as_mut().unwrap();
        let image = Image::create_from_data(
            texture.get_width(),
            texture.get_height(),
            false,
            Format::RGB8,
            &PackedByteArray::from(buffer.as_slice()),
        )
        .unwrap();
        texture.update(&image);
    }

    /*
    unsafe extern "C" fn video_display_callback(
        opaque: *mut c_void,
        picture: *mut c_void
    ) {
        unimplemented!()
    }
    */

    unsafe extern "C" fn video_format_callback(
        opaque: *mut *mut c_void,
        chroma: *mut c_char,
        width: *mut c_uint,
        height: *mut c_uint,
        pitches: *mut c_uint,
        lines: *mut c_uint,
    ) -> c_uint {
        let texture_ptr = *opaque as *mut Gd<ImageTexture>;
        let texture = texture_ptr.as_mut().unwrap();
        let img = match Image::create(*width as i32, *height as i32, false, image::Format::RGB8) {
            Some(img) => img,
            None => {
                return 0;
            }
        };
        texture.set_image(&img);
        slice::from_raw_parts_mut(chroma, 5).copy_from_slice(b"RV24\0".map(|x| x as i8).as_slice());
        let buffer = vec![0u8; (*width * *height * 3) as usize];
        *opaque = Box::into_raw(Box::new((texture_ptr, buffer))) as *mut c_void;
        *pitches = *width * 3;
        *lines = *height;
        1
    }

    unsafe extern "C" fn video_cleanup_callback(opaque: *mut c_void) {
        let data = Box::from_raw(opaque as *mut (*mut Gd<ImageTexture>, Vec<u8>));
        drop(data);
    }

    unsafe extern "C" fn audio_play_callback(
        data: *mut c_void,
        samples: *const c_void,
        count: c_uint,
        _pts: i64,
    ) {
        let audio_data = (data as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        let samples = slice::from_raw_parts(
            samples as *const f32,
            count as usize * audio_data.channels as usize,
        );
        audio_data.buffer.extend_from_slice(samples);
        audio_data.frames += count as i32;
    }

    unsafe extern "C" fn audio_pause_callback(data: *mut c_void, _pts: i64) {
        let audio_data = (data as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        audio_data.paused = true;
    }

    unsafe extern "C" fn audio_resume_callback(data: *mut c_void, _pts: i64) {
        let audio_data = (data as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        audio_data.paused = false;
    }

    unsafe extern "C" fn audio_flush_callback(data: *mut c_void, _pts: i64) {
        let audio_data = (data as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        audio_data.buffer.clear();
        audio_data.frames = 0;
    }

    unsafe extern "C" fn audio_drain_callback(_data: *mut c_void) {
        // do nothing
    }

    unsafe extern "C" fn audio_setup_callback(
        opaque: *mut *mut c_void,
        format: *mut c_char,
        rate: *mut c_uint,
        channels: *mut c_uint,
    ) -> c_int {
        let audio_data = (*opaque as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        slice::from_raw_parts_mut(format, 4).copy_from_slice(b"FL32".map(|x| x as i8).as_slice());
        audio_data.rate = *rate as i32;
        audio_data.channels = *channels as i32;
        audio_data.buffer.clear();
        audio_data.frames = 0;
        0
    }

    unsafe extern "C" fn audio_cleanup_callback(opaque: *mut c_void) {
        let audio_data = (opaque as *mut Mutex<AudioData>)
            .as_mut()
            .unwrap()
            .get_mut()
            .unwrap();
        audio_data.buffer.clear();
        audio_data.frames = 0;
    }
}

struct AudioData {
    buffer: Vec<f32>,
    frames: i32,
    rate: i32,
    channels: i32,
    paused: bool,
}

pub struct Media {
    file: *mut GFile,
    media_ptr: *mut vlc::libvlc_media_t,
}

impl Media {
    pub fn from_file(file: GFile) -> Self {
        let file = Box::into_raw(Box::new(file));
        let media_ptr = unsafe {
            vlc::libvlc_media_new_callbacks(
                Some(Self::media_open_callback),
                Some(Self::media_read_callback),
                Some(Self::media_seek_callback),
                None,
                file as *mut c_void,
            )
        };
        Self { file, media_ptr }
    }

    pub fn from_location(location: GString) -> Self {
        let file = ptr::null_mut();
        let mut location: Vec<i8> = location
            .to_utf8_buffer()
            .as_slice()
            .iter()
            .map(|x| *x as i8)
            .collect();
        location.push('\0' as i8);

        let media_ptr = unsafe {
            let location = location.as_ptr();
            vlc::libvlc_media_new_location(location)
        };
        Self { file, media_ptr }
    }

    pub fn get_media_ptr(&self) -> *mut vlc::libvlc_media_t {
        self.media_ptr
    }

    unsafe extern "C" fn media_open_callback(
        opaque: *mut c_void,
        datap: *mut *mut c_void,
        sizep: *mut u64,
    ) -> c_int {
        if let Some(file) = (opaque as *mut GFile).as_mut() {
            *datap = opaque;
            *sizep = file.length();
            let _ = file.seek(SeekFrom::Start(0));
            0
        } else {
            godot_error!("godot-vlc: unable to open media file");
            -1
        }
    }

    unsafe extern "C" fn media_read_callback(
        opaque: *mut c_void,
        buf: *mut c_uchar,
        len: usize,
    ) -> isize {
        if let Some(file) = (opaque as *mut GFile).as_mut() {
            let buf = std::slice::from_raw_parts_mut(buf, len);
            match file.read(buf) {
                Ok(n) => n.try_into().unwrap(),
                Err(_) => -1,
            }
        } else {
            -1
        }
    }

    unsafe extern "C" fn media_seek_callback(opaque: *mut c_void, offset: u64) -> c_int {
        if let Some(file) = (opaque as *mut GFile).as_mut() {
            match file.seek(SeekFrom::Start(offset)) {
                Ok(_) => 0,
                Err(_) => -1,
            }
        } else {
            -1
        }
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        unsafe {
            if !self.media_ptr.is_null() {
                vlc::libvlc_media_release(self.media_ptr);
            }
            if !self.file.is_null() {
                drop(Box::from_raw(self.file));
            }
        }
    }
}
