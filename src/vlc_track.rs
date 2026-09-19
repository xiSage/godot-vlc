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

use std::ffi::{CStr, c_char};

use crate::vlc::*;
use godot::prelude::*;

/// The string behind one of libvlc's `char *` fields.
///
/// `""` means "libvlc handed back no string, or not one this binding can decode";
/// the two are not told apart. Every one of these fields is optional: libvlc
/// fills `psz_language`, `psz_description` and the subtitle member's
/// `psz_encoding` only when it has them, and on a tracklist that came from
/// [VLCMedia] `psz_name` is always NULL.
fn c_string(field: *const c_char) -> String {
    if field.is_null() {
        return String::new();
    }
    let bytes = unsafe { CStr::from_ptr(field) }.to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap_or_default()
}

/// What one `libvlc_media_track_t` holds, as plain Rust.
///
/// No Godot type appears here on purpose: `src/acceptance.rs` reads tracks
/// through this without an engine, so the values and the union dispatch it
/// asserts on are the same code [VlcTrack::get_info] hands to GDScript.
pub(crate) struct TrackInfo {
    pub i_type: i32,
    pub codec: u32,
    pub codec_description: String,
    pub original_fourcc: u32,
    pub profile: i32,
    pub level: i32,
    pub bitrate: u32,
    pub language: String,
    pub description: String,
    pub id: String,
    pub id_stable: bool,
    pub name: String,
    pub selected: bool,
    /// The `video` member of the track's union, and `None` for every other type.
    pub video: Option<VideoInfo>,
    /// The `audio` member of the track's union, and `None` for every other type.
    pub audio: Option<AudioInfo>,
    /// The `subtitle` member of the track's union, and `None` for every other type.
    pub subtitle: Option<SubtitleInfo>,
}

/// `libvlc_video_track_t`: the `video` member of the union.
pub(crate) struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub sar_num: u32,
    pub sar_den: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub orientation: i32,
    pub projection: i32,
    pub pose_yaw: f32,
    pub pose_pitch: f32,
    pub pose_roll: f32,
    pub pose_field_of_view: f32,
    pub multiview: i32,
}

/// `libvlc_audio_track_t`: the `audio` member of the union.
pub(crate) struct AudioInfo {
    pub channels: u32,
    pub rate: u32,
}

/// `libvlc_subtitle_track_t`: the `subtitle` member of the union.
pub(crate) struct SubtitleInfo {
    pub encoding: String,
}

/// Reads one track into plain Rust values.
///
/// The union is dispatched on `i_type` and on nothing else. libvlc never writes
/// the members that do not match the type: `libvlc_media_trackpriv_new` mallocs
/// the payload and only the matching arm of `libvlc_media_trackpriv_from_es`
/// assigns a pointer to it, so the other two are memory libvlc never wrote --
/// not NULL, and not something to test for NULL either. libvlc's own
/// `libvlc_media_track_clean` releases them the same way, by switching on the
/// type rather than by looking at a pointer. `None` here therefore means "this
/// type has no such member", never "the member was empty".
#[allow(clippy::unnecessary_cast)]
pub(crate) unsafe fn read_info(ptr: *mut libvlc_media_track_t) -> TrackInfo {
    let track =
        unsafe { ptr.as_ref() }.expect("a track this binding holds is never a null pointer");
    let i_type = track.i_type as i32;

    let video = if i_type == libvlc_track_type_t_libvlc_track_video as i32 {
        unsafe { track.__bindgen_anon_1.video.as_ref() }.map(|video| VideoInfo {
            width: video.i_width,
            height: video.i_height,
            sar_num: video.i_sar_num,
            sar_den: video.i_sar_den,
            frame_rate_num: video.i_frame_rate_num,
            frame_rate_den: video.i_frame_rate_den,
            orientation: video.i_orientation as i32,
            projection: video.i_projection as i32,
            pose_yaw: video.pose.f_yaw,
            pose_pitch: video.pose.f_pitch,
            pose_roll: video.pose.f_roll,
            pose_field_of_view: video.pose.f_field_of_view,
            multiview: video.i_multiview as i32,
        })
    } else {
        None
    };

    let audio = if i_type == libvlc_track_type_t_libvlc_track_audio as i32 {
        unsafe { track.__bindgen_anon_1.audio.as_ref() }.map(|audio| AudioInfo {
            channels: audio.i_channels,
            rate: audio.i_rate,
        })
    } else {
        None
    };

    let subtitle = if i_type == libvlc_track_type_t_libvlc_track_text as i32 {
        unsafe { track.__bindgen_anon_1.subtitle.as_ref() }.map(|subtitle| SubtitleInfo {
            encoding: c_string(subtitle.psz_encoding),
        })
    } else {
        None
    };

    TrackInfo {
        i_type,
        codec: track.i_codec,
        codec_description: c_string(unsafe {
            libvlc_media_get_codec_description(track.i_type, track.i_codec)
        }),
        original_fourcc: track.i_original_fourcc,
        profile: track.i_profile,
        level: track.i_level,
        bitrate: track.i_bitrate,
        language: c_string(track.psz_language),
        description: c_string(track.psz_description),
        id: c_string(track.psz_id),
        id_stable: track.id_stable,
        name: c_string(track.psz_name),
        selected: track.selected,
        video,
        audio,
        subtitle,
    }
}

#[derive(GodotClass)]
#[class(rename=VLCTrack, no_init)]
pub struct VlcTrack {
    pub ptr: *mut libvlc_media_track_t,
}

#[allow(clippy::unnecessary_cast)]
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

    /// Top line represents top, left column left: the natural layout, and what
    /// libvlc reports when nothing said otherwise.
    #[constant]
    const ORIENT_TOP_LEFT: i32 = libvlc_video_orient_t_libvlc_video_orient_top_left as i32;
    /// Flipped horizontally.
    #[constant]
    const ORIENT_TOP_RIGHT: i32 = libvlc_video_orient_t_libvlc_video_orient_top_right as i32;
    /// Flipped vertically.
    #[constant]
    const ORIENT_BOTTOM_LEFT: i32 = libvlc_video_orient_t_libvlc_video_orient_bottom_left as i32;
    /// Rotated 180 degrees.
    #[constant]
    const ORIENT_BOTTOM_RIGHT: i32 = libvlc_video_orient_t_libvlc_video_orient_bottom_right as i32;
    /// Transposed.
    #[constant]
    const ORIENT_LEFT_TOP: i32 = libvlc_video_orient_t_libvlc_video_orient_left_top as i32;
    /// Rotated 90 degrees anti-clockwise.
    ///
    /// libvlc's own header calls this one clockwise. That header disagrees with
    /// the implementation it ships with: VLC spells this orientation
    /// `ORIENT_ROTATED_270` internally, and `ORIENT_ROTATED_90` is
    /// [constant ORIENT_RIGHT_TOP], so the two comments are the wrong way round.
    /// This binding follows the implementation.
    #[constant]
    const ORIENT_LEFT_BOTTOM: i32 = libvlc_video_orient_t_libvlc_video_orient_left_bottom as i32;
    /// Rotated 90 degrees clockwise.
    ///
    /// See [constant ORIENT_LEFT_BOTTOM] for why libvlc's header says the
    /// opposite here.
    #[constant]
    const ORIENT_RIGHT_TOP: i32 = libvlc_video_orient_t_libvlc_video_orient_right_top as i32;
    /// Anti-transposed.
    #[constant]
    const ORIENT_RIGHT_BOTTOM: i32 = libvlc_video_orient_t_libvlc_video_orient_right_bottom as i32;

    /// A flat, rectangular picture: not a projection.
    #[constant]
    const PROJECTION_RECTANGULAR: i32 =
        libvlc_video_projection_t_libvlc_video_projection_rectangular as i32;
    /// 360 spherical (equirectangular).
    #[constant]
    const PROJECTION_EQUIRECTANGULAR: i32 =
        libvlc_video_projection_t_libvlc_video_projection_equirectangular as i32;
    /// Cubemap in the standard layout.
    ///
    /// Its value is `0x100`, not `2`: the enum is not contiguous, so this cannot
    /// be treated as "the third one".
    #[constant]
    const PROJECTION_CUBEMAP_LAYOUT_STANDARD: i32 =
        libvlc_video_projection_t_libvlc_video_projection_cubemap_layout_standard as i32;

    /// No stereoscopy: a 2D picture, and what libvlc reports by default.
    #[constant]
    const MULTIVIEW_2D: i32 = libvlc_video_multiview_t_libvlc_video_multiview_2d as i32;
    /// Side-by-side stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_SBS: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_sbs as i32;
    /// Top-bottom stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_TB: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_tb as i32;
    /// Row sequential stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_ROW: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_row as i32;
    /// Column sequential stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_COL: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_col as i32;
    /// Frame sequential stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_FRAME: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_frame as i32;
    /// Checkerboard stereo, left eye first.
    #[constant]
    const MULTIVIEW_STEREO_CHECKERBOARD: i32 =
        libvlc_video_multiview_t_libvlc_video_multiview_stereo_checkerboard as i32;

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
    ///
    /// # Returns
    /// the language code, or `""` when libvlc provided none.
    #[func]
    fn get_language(&self) -> GString {
        GString::from(c_string(unsafe { self.ptr.as_ref().unwrap().psz_language }).as_str())
    }

    /// Get description.
    ///
    /// # Returns
    /// the description, or `""` when libvlc provided none.
    #[func]
    fn get_description(&self) -> GString {
        GString::from(c_string(unsafe { self.ptr.as_ref().unwrap().psz_description }).as_str())
    }

    /// Get string identifier of track, can be used to save the track preference
    /// from an other LibVLC run.
    ///
    /// # Returns
    /// the identifier, or `""` when libvlc provided none.
    #[func]
    fn get_id(&self) -> GString {
        GString::from(c_string(unsafe { self.ptr.as_ref().unwrap().psz_id }).as_str())
    }

    /// Get name of the track, only valid when the track is fetch from a
    /// [VLCMediaPlayer].
    ///
    /// # Returns
    /// the name, or `""` when libvlc provided none -- which is always, for a
    /// track that came from [method VLCMedia.get_tracklist].
    #[func]
    fn get_name(&self) -> GString {
        GString::from(c_string(unsafe { self.ptr.as_ref().unwrap().psz_name }).as_str())
    }

    /// true if the track is selected, only valid when the track is fetch from a [VLCMediaPlayer]
    #[func]
    fn is_selected(&self) -> bool {
        unsafe { self.ptr.as_ref().unwrap().selected }
    }

    pub fn from_ptr(ptr: *mut libvlc_media_track_t) -> Gd<Self> {
        Gd::from_object(Self { ptr })
    }

    /// Get codec description.
    ///
    /// # Returns
    /// libvlc's name for the codec, or `""` when it has none for this fourcc.
    #[func]
    fn get_codec_description(&self) -> GString {
        let track = unsafe { self.ptr.as_ref().unwrap() };
        GString::from(
            c_string(unsafe { libvlc_media_get_codec_description(track.i_type, track.i_codec) })
                .as_str(),
        )
    }

    /// Everything libvlc reports about this track: the whole struct in one read.
    ///
    /// # Returns
    /// a dictionary that always carries the fields every track has:
    /// - `type`: int, [constant TYPE_UNKNOWN], [constant TYPE_AUDIO],
    ///   [constant TYPE_VIDEO] or [constant TYPE_TEXT]
    /// - `codec`: int, the codec fourcc
    /// - `codec_description`: String, or `""` when libvlc has no name for that fourcc
    /// - `original_fourcc`: int
    /// - `profile`: int, or `-1` when libvlc never filled it in -- `-1` is
    ///   libvlc's own unset value for this pair, not `0`
    /// - `level`: int, or `-1` for the same reason
    /// - `bitrate`: int, `0` when it was not reported
    /// - `language`: String, or `""`
    /// - `description`: String, or `""`
    /// - `id`: String, or `""`
    /// - `id_stable`: bool
    /// - `name`: String, or `""` -- always `""` for a track that came from
    ///   [method VLCMedia.get_tracklist]
    /// - `selected`: bool, always `false` for a track that came from
    ///   [method VLCMedia.get_tracklist]
    ///
    /// and, for the one member of the union that `type` names, that member's
    /// fields and no others:
    /// - video: `width`, `height`, `sar_num`, `sar_den`, `frame_rate_num`,
    ///   `frame_rate_den`, `orientation`, `projection`, `pose_yaw`, `pose_pitch`,
    ///   `pose_roll`, `pose_field_of_view`, `multiview`
    /// - audio: `channels`, `rate`
    /// - text: `encoding`
    ///
    /// # Note
    /// - Which keys are there is the type test. A `TYPE_UNKNOWN` track carries
    ///   the common fields and no member at all, and `width` on an audio track is
    ///   absent rather than `0`, because the union member behind those keys may be
    ///   memory libvlc never wrote. libvlc itself dispatches that union on the
    ///   type, and so does this.
    /// - `width` and `height` are the *visible* size: libvlc copies
    ///   `i_visible_width`/`i_visible_height`, whatever a demuxer or a decoder had
    ///   written when the track was published -- and `0` when that was nothing.
    ///   Measured, the same file answers both ways: `test/media/h264_64x64_1s.mp4`
    ///   reports `0x0`, `sar 0/0` and a frame rate of `0/0` on a Windows machine, and
    ///   its real `64x64`, `1:1` and `10/1` on Linux, while `demo/test.mp4` reports
    ///   `854x480` and `1280:1281` on both. So these six are the file's numbers or all
    ///   zero, and which of the two you get is not something a track tells you. `0`
    ///   never means "no picture": the size of the frames comes from the video
    ///   callbacks, and [method VLCMediaPlayer.get_frame] is the frame itself. Use this
    ///   to label or to pick a track, not to size anything.
    /// - `sar_num`, `sar_den`, `frame_rate_num` and `frame_rate_den` can each be
    ///   `0`; a fraction whose denominator is `0` is unknown, not infinite.
    /// - `profile` and `level` are `-1` for a track libvlc did not fill them in for,
    ///   which is what the H.264 sample reports while `demo/test.mp4` reports `77` and
    ///   `30`: a caller that treats `0` as "unknown" reads the wrong thing.
    /// - `orientation`, `projection` and `multiview` are the video track's
    ///   declared layout, not a promise about the frames: this extension's video
    ///   paths hand a decoded picture to Godot as it is, so nothing here is
    ///   applied to it. [constant PROJECTION_EQUIRECTANGULAR] means the picture is
    ///   a 360 sphere for the caller to map, not that it is being mapped.
    /// - `pose` is the initial view point of that sphere, in degrees, and
    ///   `pose_field_of_view` is `80.0` when libvlc was not told otherwise -- the
    ///   measured value for a media that declares no view point.
    /// - `encoding` is `""` unless the subtitle's own demuxer advertised one,
    ///   which the measured answer for a plain UTF-8 `.srt` is that it does not.
    ///   So this is a clue when it is there, and silence either way.
    /// - A held track does not change: libvlc fills these fields once, when it
    ///   creates the track, and publishes a new tracklist when a decoder reports
    ///   a different format. So this dictionary and the single-field getters
    ///   cannot disagree.
    /// - Reading the fields of the wrong member is not possible here, but
    ///   asking libvlc for [constant TYPE_UNKNOWN] tracks is: on
    ///   [method VLCMedia.get_tracklist] that can return tracks whose union
    ///   libvlc never wrote. This dictionary is the same for them as for any
    ///   other unknown-track: common fields, no member.
    #[func]
    fn get_info(&self) -> VarDictionary {
        let info = unsafe { read_info(self.ptr) };
        let mut dict = VarDictionary::new();
        dict.set("type", info.i_type as i64);
        dict.set("codec", info.codec as i64);
        dict.set("codec_description", info.codec_description);
        dict.set("original_fourcc", info.original_fourcc as i64);
        dict.set("profile", info.profile as i64);
        dict.set("level", info.level as i64);
        dict.set("bitrate", info.bitrate as i64);
        dict.set("language", info.language);
        dict.set("description", info.description);
        dict.set("id", info.id);
        dict.set("id_stable", info.id_stable);
        dict.set("name", info.name);
        dict.set("selected", info.selected);

        if let Some(video) = info.video {
            dict.set("width", video.width as i64);
            dict.set("height", video.height as i64);
            dict.set("sar_num", video.sar_num as i64);
            dict.set("sar_den", video.sar_den as i64);
            dict.set("frame_rate_num", video.frame_rate_num as i64);
            dict.set("frame_rate_den", video.frame_rate_den as i64);
            dict.set("orientation", video.orientation as i64);
            dict.set("projection", video.projection as i64);
            dict.set("pose_yaw", video.pose_yaw as f64);
            dict.set("pose_pitch", video.pose_pitch as f64);
            dict.set("pose_roll", video.pose_roll as f64);
            dict.set("pose_field_of_view", video.pose_field_of_view as f64);
            dict.set("multiview", video.multiview as i64);
        }
        if let Some(audio) = info.audio {
            dict.set("channels", audio.channels as i64);
            dict.set("rate", audio.rate as i64);
        }
        if let Some(subtitle) = info.subtitle {
            dict.set("encoding", subtitle.encoding);
        }
        dict
    }
}

impl Drop for VlcTrack {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_track_release(self.ptr);
        }
    }
}
