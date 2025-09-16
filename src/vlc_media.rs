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

use std::{
    ffi::{c_int, c_uchar, c_void, CStr},
    io::{Read, Seek, SeekFrom},
    ptr, slice,
};

use crate::{util::cstring_from_gstring, vlc::*, vlc_instance, vlc_track_list::VlcTrackList};
use godot::{
    classes::{file_access::ModeFlags, WeakRef},
    global::weakref,
    prelude::*,
};

#[derive(GodotClass)]
#[class(base=Resource, rename=VLCMedia, no_init)]
pub struct VlcMedia {
    base: Base<Resource>,
    #[allow(dead_code)]
    path: Option<Box<GString>>,
    pub media_ptr: *mut libvlc_media_t,
    self_gd: Option<Box<Gd<WeakRef>>>,
}

#[godot_api]
impl VlcMedia {
    #[constant]
    const PARSED_STATUS_NONE: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_none as i32;
    #[constant]
    const PARSED_STATUS_PENDING: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_pending as i32;
    #[constant]
    const PARSED_STATUS_SKIPPED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_skipped as i32;
    #[constant]
    const PARSED_STATUS_FAILED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_failed as i32;
    #[constant]
    const PARSED_STATUS_TIMEOUT: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_timeout as i32;
    #[constant]
    const PARSED_STATUS_CANCELLED: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_cancelled as i32;
    #[constant]
    const PARSED_STATUS_DONE: i32 =
        libvlc_media_parsed_status_t_libvlc_media_parsed_status_done as i32;

    /// Parse media if it's a local file.
    #[constant]
    const PARSE_FLAG_PARSE_LOCAL: i32 = libvlc_media_parse_flag_t_libvlc_media_parse_local as i32;
    /// Parse media even if it's a network file.
    #[constant]
    const PARSE_FLAG_PARSE_NETWORK: i32 =
        libvlc_media_parse_flag_t_libvlc_media_parse_network as i32;
    /// Force parsing the media even if it would be skipped.
    #[constant]
    const PARSE_FLAG_PARSE_FORCED: i32 = libvlc_media_parse_flag_t_libvlc_media_parse_forced as i32;
    /// Fetch meta and cover art using local resources.
    #[constant]
    const PARSE_FLAG_FETCH_LOCAL: i32 = libvlc_media_parse_flag_t_libvlc_media_fetch_local as i32;
    /// Fetch meta and cover art using network resources.
    #[constant]
    const PARSE_FLAG_FETCH_NETWORK: i32 =
        libvlc_media_parse_flag_t_libvlc_media_fetch_network as i32;
    /// Interact with the user (via libvlc_dialog_cbs) when preparsing this item (and not its sub items).
    ///
    /// Set this flag in order to receive a callback when the input is asking for credentials.
    #[constant]
    const PARSE_FLAG_DO_INTERACT: i32 = libvlc_media_parse_flag_t_libvlc_media_do_interact as i32;

    #[constant]
    const META_TITLE: i32 = libvlc_meta_t_libvlc_meta_Title as i32;
    #[constant]
    const META_ARTIST: i32 = libvlc_meta_t_libvlc_meta_Artist as i32;
    #[constant]
    const META_GENRE: i32 = libvlc_meta_t_libvlc_meta_Genre as i32;
    #[constant]
    const META_COPYRIGHT: i32 = libvlc_meta_t_libvlc_meta_Copyright as i32;
    #[constant]
    const META_ALBUM: i32 = libvlc_meta_t_libvlc_meta_Album as i32;
    #[constant]
    const META_TRACK_NUMBER: i32 = libvlc_meta_t_libvlc_meta_TrackNumber as i32;
    #[constant]
    const META_DESCRIPTION: i32 = libvlc_meta_t_libvlc_meta_Description as i32;
    #[constant]
    const META_RATING: i32 = libvlc_meta_t_libvlc_meta_Rating as i32;
    #[constant]
    const META_DATE: i32 = libvlc_meta_t_libvlc_meta_Date as i32;
    #[constant]
    const META_SETTING: i32 = libvlc_meta_t_libvlc_meta_Setting as i32;
    #[constant]
    const META_URL: i32 = libvlc_meta_t_libvlc_meta_URL as i32;
    #[constant]
    const META_LANGUAGE: i32 = libvlc_meta_t_libvlc_meta_Language as i32;
    #[constant]
    const META_NOW_PLAYING: i32 = libvlc_meta_t_libvlc_meta_NowPlaying as i32;
    #[constant]
    const META_PUBLISHER: i32 = libvlc_meta_t_libvlc_meta_Publisher as i32;
    #[constant]
    const META_ENCODED_BY: i32 = libvlc_meta_t_libvlc_meta_EncodedBy as i32;
    #[constant]
    const META_ARTWORK_URL: i32 = libvlc_meta_t_libvlc_meta_ArtworkURL as i32;
    #[constant]
    const META_TRACK_ID: i32 = libvlc_meta_t_libvlc_meta_TrackID as i32;
    #[constant]
    const META_TRACK_TOTAL: i32 = libvlc_meta_t_libvlc_meta_TrackTotal as i32;
    #[constant]
    const META_DIRECTOR: i32 = libvlc_meta_t_libvlc_meta_Director as i32;
    #[constant]
    const META_SEASON: i32 = libvlc_meta_t_libvlc_meta_Season as i32;
    #[constant]
    const META_EPISODE: i32 = libvlc_meta_t_libvlc_meta_Episode as i32;
    #[constant]
    const META_SHOW_NAME: i32 = libvlc_meta_t_libvlc_meta_ShowName as i32;
    #[constant]
    const META_ACTORS: i32 = libvlc_meta_t_libvlc_meta_Actors as i32;
    #[constant]
    const META_ALBUM_ARTIST: i32 = libvlc_meta_t_libvlc_meta_AlbumArtist as i32;
    #[constant]
    const DISC_NUMBER: i32 = libvlc_meta_t_libvlc_meta_DiscNumber as i32;
    #[constant]
    const DISC_TOTAL: i32 = libvlc_meta_t_libvlc_meta_DiscTotal as i32;

    /// Parsing state of a `VLCMedia` changed.
    #[signal]
    fn parsed_changed(status: i32);

    /// Create a new `VLCMedia` from a file path.
    ///
    /// # Parameters
    /// - [param path] the path to the media file.
    #[func]
    fn load_from_file(path: GString) -> Gd<Self> {
        let mut path = Box::new(path);
        let media_ptr = unsafe {
            libvlc_media_new_callbacks(
                Some(media_open_callback),
                Some(media_read_callback),
                Some(media_seek_callback),
                Some(media_close_cb),
                path.as_mut() as *mut _ as *mut c_void,
            )
        };
        let mut media = Gd::from_init_fn(|base| Self {
            base,
            path: Some(path),
            media_ptr,
            self_gd: None,
        });
        let self_gd = Box::new(weakref(&media.to_variant()).to::<Gd<WeakRef>>());
        media.bind_mut().self_gd = Some(self_gd);

        Self::register_signals(&mut media);

        media
    }

    /// Create a new `VLCMedia` from a media resource locator (MRL).\
    /// A media resource locator (MRL) is a string of characters used to identify a multimedia resource or part of a multimedia resource. A MRL may be used to identify inputs or outputs to VLC media player. See [VideoLAN wiki](https://wiki.videolan.org/Media_resource_locator).
    ///
    /// # Parameters
    /// - [param mrl] the media resource locator.
    #[func]
    fn load_from_mrl(mrl: GString) -> Option<Gd<Self>> {
        let mrl = cstring_from_gstring(mrl);
        let media_ptr = unsafe { libvlc_media_new_location(mrl.as_ptr()) };
        if media_ptr.is_null() {
            return None;
        }
        let mut media = Gd::from_init_fn(|base| Self {
            base,
            path: None,
            media_ptr,
            self_gd: None,
        });
        let self_gd = Box::new(weakref(&media.to_variant()).to::<Gd<WeakRef>>());
        media.bind_mut().self_gd = Some(self_gd);

        Self::register_signals(&mut media);

        Some(media)
    }

    fn register_signals(media: &mut Gd<Self>) {
        unsafe {
            fn get_media(ptr: *mut c_void) -> Gd<VlcMedia> {
                unsafe { (ptr as *mut WeakRef).as_mut().unwrap().get_ref().to() }
            }

            let event_manager = libvlc_media_event_manager(media.bind().media_ptr);

            unsafe extern "C" fn parsed_changed_callback(
                _event: *const libvlc_event_t,
                user_data: *mut c_void,
            ) {
                let mut media = get_media(user_data);
                let status = libvlc_media_get_parsed_status(media.bind().media_ptr);
                media.call_deferred(
                    "emit_signal",
                    &[
                        StringName::from(c"parsed_changed").to_variant(),
                        status.to_variant(),
                    ],
                );
            }
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaParsedChanged as libvlc_event_type_t,
                Some(parsed_changed_callback),
                media.bind_mut().self_gd.as_mut().unwrap().as_mut() as *mut _ as *mut c_void,
            );
        }
    }

    /// Get duration (in ms) of media descriptor object item.\
    /// Note, you need to call [method parse_request] or play the media at least once before calling this function. Not doing this will result in an undefined result.
    ///
    /// # Returns
    /// duration of media item or -1 on error
    #[func]
    fn get_duration(&self) -> i64 {
        unsafe { libvlc_media_get_duration(self.media_ptr) }
    }

    /// Read the meta of the media.\
    /// Note, you need to call [method parse_request] or play the media at least once before calling this function. If the media has not yet been parsed this will return an empty string.
    ///
    /// # Parameters
    /// - [param meta] the media descriptor
    ///
    /// # Returns
    /// the media's meta
    #[func]
    fn get_meta(&self, meta: u32) -> GString {
        let str =
            unsafe { CStr::from_ptr(libvlc_media_get_meta(self.media_ptr, meta as libvlc_meta_t)) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// Read the meta extra of the media.\
    /// If the media has not yet been parsed this will return an empty string.
    ///
    /// # Parameters
    /// - [param name] the meta extra to read (nonnullable)
    ///
    /// # Returns
    /// the media's meta extra
    #[func]
    fn get_meta_extra(&self, name: GString) -> GString {
        let name = cstring_from_gstring(name);
        let str =
            unsafe { CStr::from_ptr(libvlc_media_get_meta_extra(self.media_ptr, name.as_ptr())) };
        GString::try_from_cstr(str, Encoding::Utf8).unwrap_or_default()
    }

    /// Read the meta extra names of the media.
    ///
    /// # Returns
    /// the media's meta extra name array
    #[func]
    fn get_meta_extra_names(&self) -> PackedStringArray {
        let names = ptr::null_mut();
        let count = unsafe { libvlc_media_get_meta_extra_names(self.media_ptr, names) };
        let arr = unsafe {
            if count > 0 {
                slice::from_raw_parts(*names, count as usize)
                    .iter()
                    .map(|x| {
                        GString::try_from_cstr(CStr::from_ptr(*x), Encoding::Utf8)
                            .unwrap_or_default()
                    })
                    .collect()
            } else {
                PackedStringArray::default()
            }
        };
        unsafe {
            libvlc_media_meta_extra_names_release(*names, count);
        };
        arr
    }

    /// Get Parsed status for media.
    ///
    /// # Returns
    /// parsed status of media ([constant PARSED_STATUS_NONE], [constant PARSED_STATUS_PENDING], [constant PARSED_STATUS_SKIPPED],...)
    #[func]
    fn get_parsed_status(&self) -> i32 {
        unsafe { libvlc_media_get_parsed_status(self.media_ptr) as i32 }
    }

    /// Get the current statistics about the media.
    ///
    /// # Returns
    /// dictionary that contain the statistics about the media or an empty dictionary if the statistics are not available. The dictionary contains the following keys:
    /// - `read_bytes`: int
    /// - `input_bitrate`: float
    /// - `demux_read_bytes`: int
    /// - `demux_bitrate`: float
    /// - `demux_corrupted`: int
    /// - `demux_discontinuity`: int
    /// - `decoded_video`: int
    /// - `decoded_audio`: int
    /// - `displayed_pictures`: int
    /// - `late_pictures`: int
    /// - `lost_pictures`: int
    /// - `played_abuffers`: int
    /// - `lost_abuffers`: int
    #[func]
    fn get_stats(&self) -> Dictionary {
        let mut stats = libvlc_media_stats_t {
            i_read_bytes: 0,
            f_input_bitrate: 0.0,
            i_demux_read_bytes: 0,
            f_demux_bitrate: 0.0,
            i_demux_corrupted: 0,
            i_demux_discontinuity: 0,
            i_decoded_video: 0,
            i_decoded_audio: 0,
            i_displayed_pictures: 0,
            i_late_pictures: 0,
            i_lost_pictures: 0,
            i_played_abuffers: 0,
            i_lost_abuffers: 0,
        };
        let available = unsafe { libvlc_media_get_stats(self.media_ptr, &mut stats) };
        if available {
            let mut dict = Dictionary::new();
            dict.set("read_bytes", stats.i_read_bytes);
            dict.set("input_bitrate", stats.f_input_bitrate);
            dict.set("demux_read_bytes", stats.i_demux_read_bytes);
            dict.set("demux_bitrate", stats.f_demux_bitrate);
            dict.set("demux_corrupted", stats.i_demux_corrupted);
            dict.set("demux_discontinuity", stats.i_demux_discontinuity);
            dict.set("decoded_video", stats.i_decoded_video);
            dict.set("decoded_audio", stats.i_decoded_audio);
            dict.set("displayed_pictures", stats.i_displayed_pictures);
            dict.set("late_pictures", stats.i_late_pictures);
            dict.set("lost_pictures", stats.i_lost_pictures);
            dict.set("played_abuffers", stats.i_played_abuffers);
            dict.set("lost_abuffers", stats.i_lost_abuffers);
            dict
        } else {
            Dictionary::default()
        }
    }

    /// Get the track list for one type.
    ///
    /// # Note
    /// You need to call [method parse_request] or play the media at least once before calling this function. Not doing this will result in an empty list.
    ///
    /// # Parameters
    /// - [param track_type] type of the track list to request (e.g. [constant TRACK_TYPE_VIDEO], [constant TRACK_TYPE_AUDIO], [constant TRACK_TYPE_TEXT])
    ///
    /// # Returns
    /// a valid [VLCTrackList] or null in case of error, if there is no track for a category, the returned list will have a size of 0.
    #[func]
    fn get_tracklist(&self, track_type: i32) -> Option<Gd<VlcTrackList>> {
        unsafe { VlcTrackList::from_ptr(libvlc_media_get_tracklist(self.media_ptr, track_type)) }
    }

    /// Parse the media asynchronously with options.\
    /// This fetches (local or network) art, meta data and/or tracks information.\
    /// To track when this is over you can listen to [signal parsed_changed] signal. However if this functions returns an error, you will not receive any events.\
    /// It uses a flag to specify parse options ([constant PARSE_FLAG_PARSE_LOCAL], [constant PARSE_FLAG_PARSE_NETWORK],...). All these flags can be combined. By default, media is parsed if it's a local file.
    ///
    /// # Note
    /// Parsing can be aborted with [method parse_stop].
    ///
    /// # Parameters
    /// - [param parse_flag] parse options:
    /// - [param timeout] maximum time allowed to preparse the media. If -1, the default "preparse-timeout" option will be used as a timeout. If 0, it will wait indefinitely. If > 0, the timeout will be used (in milliseconds).
    ///
    /// # Returns
    /// -1 in case of error, 0 otherwise
    #[func]
    fn parse_request(&mut self, parse_flag: i32, timeout: i32) -> i32 {
        unsafe {
            libvlc_media_parse_request(
                vlc_instance::get(),
                self.media_ptr,
                parse_flag as libvlc_media_parse_flag_t
                    | libvlc_media_parse_flag_t_libvlc_media_fetch_local,
                timeout,
            )
        }
    }

    /// Stop the parsing of the media.\
    /// When the media parsing is stopped, the [signal parsed_changed] signal will be sent with the [constant PARSED_STATUS_TIMEOUT] status.
    #[func]
    fn parse_stop(&mut self) {
        unsafe {
            libvlc_media_parse_stop(vlc_instance::get(), self.media_ptr);
        }
    }
}

impl Drop for VlcMedia {
    fn drop(&mut self) {
        unsafe {
            if !self.media_ptr.is_null() {
                libvlc_media_release(self.media_ptr);
            }
        }
    }
}

unsafe extern "C" fn media_open_callback(
    opaque: *mut c_void,
    datap: *mut *mut c_void,
    sizep: *mut u64,
) -> c_int {
    if let Some(path) = (opaque as *mut GString).as_ref() {
        if let Ok(mut file) = GFile::open(path, ModeFlags::READ) {
            *sizep = file.length();
            let _ = file.seek(SeekFrom::Start(0));
            *datap = Box::into_raw(Box::new(file)) as *mut c_void;
            return 0;
        }
    }
    godot_error!("godot-vlc: unable to open media file");
    -1
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

unsafe extern "C" fn media_close_cb(opaque: *mut ::std::os::raw::c_void) {
    if !opaque.is_null() {
        drop(Box::from_raw(opaque as *mut GFile));
    }
}
