use std::ffi::{CStr, CString};

use crate::vlc::*;
use godot::prelude::*;

#[derive(GodotClass)]
#[class(rename=VLCTrack, no_init)]
pub struct VlcTrack {
    pub ptr: *mut libvlc_media_track_t,
}

#[godot_api]
impl VlcTrack {
    #[constant]
    const TYPE_UNKNOWN: i32 = libvlc_track_type_t_libvlc_track_unknown;
    #[constant]
    const TYPE_AUDIO: i32 = libvlc_track_type_t_libvlc_track_audio;
    #[constant]
    const TYPE_VIDEO: i32 = libvlc_track_type_t_libvlc_track_video;
    #[constant]
    const TYPE_TEXT: i32 = libvlc_track_type_t_libvlc_track_text;

    /// Get the track type. ([constant TYPE_AUDIO], [constant TYPE_VIDEO],...])
    #[func]
    fn get_type(&self) -> i32 {
        unsafe { self.ptr.as_ref().unwrap().i_type }
    }

    /// Get birrate.
    #[func]
    fn get_bitrate(&self) -> u32 {
        unsafe { self.ptr.as_ref().unwrap().i_bitrate }
    }

    /// Get language.
    #[func]
    fn get_language(&self) -> GString {
        let str = unsafe { CStr::from_ptr(self.ptr.as_ref().unwrap().psz_language) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// Get description.
    #[func]
    fn get_description(&self) -> GString {
        let str = unsafe { CStr::from_ptr(self.ptr.as_ref().unwrap().psz_description) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// Get string identifier of track, can be used to save the track preference from an other LibVLC run.
    #[func]
    fn get_id(&self) -> GString {
        let str = unsafe { CStr::from_ptr(self.ptr.as_ref().unwrap().psz_id) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// Get name of the track, only valid when the track is fetch from a [VLCMediaPlayer].
    #[func]
    fn get_name(&self) -> GString {
        let str = unsafe { CStr::from_ptr(self.ptr.as_ref().unwrap().psz_name) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// true if the track is selected, only valid when the track is fetch from a [VLCMediaPlayer]
    #[func]
    fn is_selected(&self) -> bool {
        unsafe { self.ptr.as_ref().unwrap().selected }
    }

    pub fn from_ptr(ptr: *mut libvlc_media_track_t) -> Gd<Self> {
        Gd::from_object(Self { ptr })
    }
}

impl Drop for VlcTrack {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_track_release(self.ptr);
        }
    }
}
