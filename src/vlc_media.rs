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
    ffi::{CStr, c_int, c_uchar, c_uint, c_void},
    io::{Read, Seek, SeekFrom},
    ptr, slice,
};

use crate::{
    util::cstring_from_gstring,
    vlc::*,
    vlc_instance::{self, clear_last_error, last_error},
    vlc_media_list::VlcMediaList,
    vlc_subtitle::VlcSubtitle,
    vlc_track_list::VlcTrackList,
};
use godot::{
    classes::{WeakRef, file_access::ModeFlags},
    global::weakref,
    prelude::*,
};

/// A media descriptor: the thing a [VLCMediaPlayer] plays, and what it was built
/// from.
///
/// # Where a media comes from
/// - [method load_from_file] reads a file through **Godot's own filesystem**, so a
///   media inside `res://` works in an exported project where there is no path
///   libvlc could open. The input is an in-memory one (`imem://`), which is why
///   per-media options that belong to an access module do nothing on it; see
///   [method add_option].
/// - [method load_from_mrl] hands a MRL to libvlc unchanged.
/// - A scene can reference a media file directly: the class is a [Resource], and
///   the extension registers the loader that turns `res://movie.mp4` into one.
///
/// # Asking a media what it is
/// There is no single answer, and the three that exist disagree on purpose:
/// - [method get_mrl] is libvlc's answer. Every media built by
///   [method load_from_file] answers the literal `"imem://"`, whatever file is
///   behind it, so it identifies the *kind* of input and not the file.
/// - [member Resource.resource_path] is the engine's answer, and it is set for a
///   media that arrived through the resource loader (a scene's `ext_resource`, or
///   `load("res://movie.mp4")`) -- it is the only one of these that names the file
///   for that case, and it is empty for a media built by a direct call.
/// - [method get_source_path] does not exist, on purpose: for a media a script
///   built itself, the script passed the string in and can keep it. Nothing here
///   remembers it, so a media that is handed to a player and read back later is
///   anonymous unless its creator kept the string.
///
/// [method get_type] answers what libvlc made of the media, and
/// [method duplicate_media] makes an independent copy of it.
#[derive(GodotClass)]
#[class(base=Resource, rename=VLCMedia, no_init)]
pub struct VlcMedia {
    base: Base<Resource>,
    #[allow(dead_code)]
    path: Option<Box<GString>>,
    pub media_ptr: *mut libvlc_media_t,
    self_gd: Option<Box<Gd<WeakRef>>>,
}

#[allow(clippy::unnecessary_cast)]
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

    /// Apply the option even where LibVLC would otherwise refuse it as unsafe.
    /// [method add_option] always sets this; [method add_option_flag] does not.
    #[constant]
    const OPTION_TRUSTED: i32 = libvlc_media_option_trusted as i32;
    /// Ignore the option when the same option string is already on this media.
    /// [method add_option] always sets this; [method add_option_flag] does not.
    #[constant]
    const OPTION_UNIQUE: i32 = libvlc_media_option_unique as i32;

    /// The type an external subtitle is added with.
    /// LibVLC forces its `subtitle` demux, whatever the file is called and whatever
    /// a `:demux=` option says.
    #[constant]
    const SLAVE_TYPE_SUBTITLE: i32 =
        libvlc_media_slave_type_t_libvlc_media_slave_type_subtitle as i32;
    /// The type any other external source is added with -- LibVLC's own header also
    /// calls it `libvlc_media_slave_type_audio`, which is this same value under
    /// another name. Nothing forces a demux for it, so what the source contains is
    /// decided by its content.
    #[constant]
    const SLAVE_TYPE_GENERIC: i32 =
        libvlc_media_slave_type_t_libvlc_media_slave_type_generic as i32;

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

    /// [method get_type] answers this for a media libvlc could not make anything
    /// of. Every media built by [method load_from_file] answers it, because the
    /// in-memory access it is built on has no scheme libvlc recognises.
    #[constant]
    const MEDIA_TYPE_UNKNOWN: i32 = libvlc_media_type_t_libvlc_media_type_unknown as i32;
    /// A file.
    #[constant]
    const MEDIA_TYPE_FILE: i32 = libvlc_media_type_t_libvlc_media_type_file as i32;
    /// A directory. A **path** to a directory answers [constant MEDIA_TYPE_FILE]
    /// instead; this is what a `dir://` MRL is.
    #[constant]
    const MEDIA_TYPE_DIRECTORY: i32 = libvlc_media_type_t_libvlc_media_type_directory as i32;
    /// An optical disc.
    #[constant]
    const MEDIA_TYPE_DISC: i32 = libvlc_media_type_t_libvlc_media_type_disc as i32;
    /// A stream: a protocol libvlc does not treat as a file, such as `udp://`.
    #[constant]
    const MEDIA_TYPE_STREAM: i32 = libvlc_media_type_t_libvlc_media_type_stream as i32;
    /// A playlist. A local `.m3u` answers [constant MEDIA_TYPE_FILE] until it has
    /// been parsed, and this afterwards; see [method get_type].
    #[constant]
    const MEDIA_TYPE_PLAYLIST: i32 = libvlc_media_type_t_libvlc_media_type_playlist as i32;

    /// Parsing state of a `VLCMedia` changed.
    #[signal]
    fn parsed_changed(status: i32);

    /// Create a new `VLCMedia` from a file path.
    ///
    /// The file is read through **Godot's own filesystem**, not handed to libvlc as
    /// a path: media inside `res://` live in the PCK, where there is no path libvlc
    /// could open. What libvlc gets is an in-memory input, so this media answers
    /// `"imem://"` to [method get_mrl], [constant MEDIA_TYPE_UNKNOWN] to
    /// [method get_type], and nothing to [member Resource.resource_path] unless it
    /// arrived through the resource loader -- see the class documentation for which
    /// of the three names this file.
    ///
    /// # Parameters
    /// - [param path] the path to the media file.
    #[func]
    pub fn load_from_file(path: GString) -> Gd<Self> {
        let mut path = Box::new(path);
        // The reason for a failure is not in the return value -- libvlc answers
        // with a null pointer and keeps the explanation on the side -- so the side
        // is cleared first and read only if this call turns out to be the one that
        // failed. See `clear_last_error`.
        clear_last_error();
        let media_ptr = unsafe {
            libvlc_media_new_callbacks(
                Some(media_open_callback),
                Some(media_read_callback),
                Some(media_seek_callback),
                Some(media_close_cb),
                path.as_mut() as *mut _ as *mut c_void,
            )
        };
        assert!(
            !media_ptr.is_null(),
            "libvlc could not create a media for {path}: {}",
            last_error()
        );
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
    /// The MRL is handed to libvlc unchanged, so it is also what [method get_mrl]
    /// reads back, and [method get_type] is libvlc's guess from its scheme.
    ///
    /// # Parameters
    /// - [param mrl] the media resource locator.
    #[func]
    fn load_from_mrl(mrl: GString) -> Option<Gd<Self>> {
        let mrl = cstring_from_gstring(mrl);
        clear_last_error();
        let media_ptr = unsafe { libvlc_media_new_location(mrl.as_ptr()) };
        if media_ptr.is_null() {
            godot_error!(
                "godot-vlc: libvlc refused the MRL {}: {}",
                mrl.to_string_lossy(),
                last_error()
            );
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

    /// Wraps a media libvlc hands over, taking the reference that came with it.
    ///
    /// [method duplicate_media] uses this for a copy, and a media list uses it for
    /// every element it hands out -- `libvlc_media_list_item_at_index` retains what it
    /// returns, and that reference is what the wrapper owns and `Drop` releases. The
    /// parse signal is registered like a media this binding built, so a media that
    /// arrived this way behaves the same in a script.
    pub(crate) fn from_ptr(media_ptr: *mut libvlc_media_t) -> Option<Gd<Self>> {
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

    /// The MRL libvlc holds for this media.
    ///
    /// # Returns
    /// the string libvlc reports, or `""` when it has none -- which through the
    /// constructors here means only that libvlc could not copy it.
    ///
    /// # Note
    /// - It is libvlc's own string, copied before the buffer libvlc allocated for
    ///   it is freed, so the caller owns nothing and can read it as an ordinary
    ///   `String`.
    /// - **Every media built by [method load_from_file] answers `"imem://"`**, the
    ///   same constant for every file, because that is the access it was built on.
    ///   It is not an identifier: use [member Resource.resource_path] for a media
    ///   that came through the resource loader, or keep the path the script passed
    ///   in. See the class documentation.
    /// - A media built from a **path** answers a `file://` URI, and libvlc
    ///   percent-encodes it -- `test/media/h264_64x64_1s.mp4` becomes
    ///   `test%2Fmedia%2Fh264_64x64_1s.mp4` in the path part -- so it does not
    ///   compare equal to the path that was passed in. A media built from a MRL
    ///   answers that MRL verbatim.
    /// - It does not change: parsing or playing a media does not rewrite it. For a
    ///   local `.m3u` it stays the path it was given, even once
    ///   [method get_type] has started answering [constant MEDIA_TYPE_PLAYLIST].
    #[func]
    fn get_mrl(&self) -> GString {
        let mrl = unsafe { libvlc_media_get_mrl(self.media_ptr) };
        if mrl.is_null() {
            return GString::new();
        }
        let copied = crate::vlc_track::c_string(mrl);
        // libvlc hands over a copy of its own string -- a `strdup`, with no release
        // function of its own -- so this is the one place in the binding that frees
        // what libvlc returned.
        unsafe { libvlc_free(mrl as *mut c_void) };
        GString::from(copied.as_str())
    }

    /// What libvlc made of this media: a file, a directory, a disc, a stream, a
    /// playlist, or nothing it recognises.
    ///
    /// # Returns
    /// one of the `MEDIA_TYPE_*` constants.
    ///
    /// # Note
    /// - It is decided **from the MRL's scheme** when the media is built
    ///   ([method load_from_mrl] and [method load_from_file] both go through it),
    ///   and it **can change later**: when the media is parsed, the demuxer's own
    ///   answer replaces it. A local `.m3u` is [constant MEDIA_TYPE_FILE] before
    ///   that and [constant MEDIA_TYPE_PLAYLIST] after. So the value describes the
    ///   media as libvlc knows it *now*, not what it was built as.
    /// - A **path** to a directory answers [constant MEDIA_TYPE_FILE]; only a
    ///   `dir://` MRL is [constant MEDIA_TYPE_DIRECTORY].
    /// - Every media from [method load_from_file] answers
    ///   [constant MEDIA_TYPE_UNKNOWN] and keeps answering it: the in-memory access
    ///   has no scheme to guess from. That has a second consequence: libvlc refuses
    ///   to parse a media it cannot type, so [method parse_request] reports
    ///   [constant PARSED_STATUS_SKIPPED] for one unless the flags include
    ///   [constant PARSE_FLAG_FORCED].
    /// - It is read under libvlc's own lock, but the answer can still be the
    ///   pre-parse one if a parse is running: the value moves, and this call cannot
    ///   wait for it.
    #[func]
    fn get_type(&self) -> i32 {
        unsafe { libvlc_media_get_type(self.media_ptr) as i32 }
    }

    /// A copy of this media, independent of it.
    ///
    /// This is `libvlc_media_duplicate`: the copy is a media of its own, and
    /// changing one does not change the other. It is the way to hand the same media
    /// to two players without either of them sharing the other's state.
    ///
    /// # Returns
    /// a [VLCMedia] the caller owns, or `null` if libvlc could not copy it.
    ///
    /// # Note
    /// - **Copied**: the MRL, the name, the duration, the type, the metadata, the
    ///   options added with [method add_option] or [method add_option_flag], the
    ///   subtitle slaves, and the parsed tracks.
    /// - **Not copied**: the parse status (the copy starts unparsed, whatever the
    ///   original was), any user data libvlc holds, and the subitems of a playlist
    ///   -- a copy of a playlist starts with an empty one, and getting its entries
    ///   would mean parsing it again.
    /// - [method get_mrl] on the copy answers what it answers on the original,
    ///   `"imem://"` included: the copy is as anonymous as its source, and
    ///   [member Resource.resource_path] is empty on it.
    /// - The copy is a new media, so [method VLCMediaPlayer.set_media] can be
    ///   pointed at it without disturbing a player using the original.
    #[func]
    fn duplicate_media(&self) -> Option<Gd<Self>> {
        clear_last_error();
        let copy = unsafe { libvlc_media_duplicate(self.media_ptr) };
        if copy.is_null() {
            godot_error!(
                "godot-vlc: libvlc could not copy the media {}: {}",
                self.get_mrl(),
                last_error()
            );
            return None;
        }
        Self::from_ptr(copy)
    }

    fn register_signals(media: &mut Gd<Self>) {
        unsafe {
            let event_manager = libvlc_media_event_manager(media.bind().media_ptr);
            libvlc_event_attach(
                event_manager,
                libvlc_event_e_libvlc_MediaParsedChanged as libvlc_event_type_t,
                Some(parsed_changed_callback),
                Self::event_data(media),
            );
        }
    }

    /// What libvlc is handed as a media event's user data: the object pointer of the weak
    /// reference.
    ///
    /// The same pointer has to come back for the detach in `Drop`, which is why it is
    /// computed in one place -- and it has to be the *object* pointer, not the address of
    /// the `Gd` handle that wraps it: the callback casts it to `*mut WeakRef` and calls
    /// into it, so a handle's address is not a pointer to the object at all.
    fn event_data(media: &mut Gd<Self>) -> *mut c_void {
        match media.bind_mut().self_gd.as_deref_mut() {
            Some(weak) => (&mut **weak) as *mut WeakRef as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    /// The media's own subitems: the entries a playlist, a disc or a directory holds.
    ///
    /// # Returns
    /// the list of what is inside this media. It is **read-only** (see
    /// [method VLCMediaList.is_read_only]) and **live**: libvlc's parsing thread
    /// appends to it as it finds entries, so its contents change under the caller.
    /// [signal VLCMediaList.end_reached] is how a script waits for that to finish.
    ///
    /// # Note
    /// - Nothing is in it until the media has been parsed, and **playing is not
    ///   parsing**: the entries of a playlist do arrive while it plays, but
    ///   [signal VLCMediaList.end_reached] only comes from
    ///   [method parse_request] (measured: libvlc sends it where it reports a media's
    ///   parsed status changing). A `.m3u` is [constant MEDIA_TYPE_FILE] until a parse
    ///   turns it into [constant MEDIA_TYPE_PLAYLIST], and for a media whose type
    ///   libvlc cannot guess (one built by [method load_from_file]) the flags have to
    ///   include [constant PARSE_FORCED].
    /// - The wrapper owns a reference of its own, so the list stays valid even if the
    ///   media is freed first.
    /// - libvlc's header for this call says it can answer `NULL`, and for a media this
    ///   binding built it cannot: the list is created with the media, and libvlc hands
    ///   back the same one every time. `null` here would mean the media was not one of
    ///   this binding's.
    #[func]
    fn get_subitems(&self) -> Option<Gd<VlcMediaList>> {
        // libvlc retains the list for the caller, and `from_ptr` takes that over.
        let subitems = unsafe { libvlc_media_subitems(self.media_ptr) };
        VlcMediaList::from_ptr(subitems)
    }

    /// Add an option to the media.\
    /// The option goes onto the media descriptor -- always with [constant OPTION_UNIQUE] and [constant OPTION_TRUSTED], which is what [method add_option_flag] exists to change -- and LibVLC reads the options back once, when the input is created.
    ///
    /// # When an option is read
    /// Assigning the media to a [VLCMediaPlayer] creates that input immediately: `play()` is not what starts an input, so the last moment an option can be added is **before** the assignment. `VLCMedia.load_from_mrl(...)`, then `add_option(":start-time=10")`, then `player.media = media` is the order that works. An option added after the assignment is not read for the playback that is running or about to start; it is read again only when the input is rebuilt, which the same media gets on the next [method VLCMediaPlayer.play] that follows a [method VLCMediaPlayer.stop_async]. Media that arrives as a `res://` resource is assigned by the scene itself, so there is no moment left to call this on it -- build the media with [method load_from_file] or [method load_from_mrl] when it has to carry options.
    ///
    /// # What the options are
    /// The names are VLC's own (`vlc --longhelp` lists them), and a leading `:` is optional. Nothing here validates them: an unknown name, a value that does not parse and a value outside the range its configuration option declares are all discarded without a word, at any log level -- `":start-time=abc"` is applied as `0`, and a misspelled name is never reported. A per-media value also bypasses the range the configuration declares rather than being clamped to it.
    ///
    /// An option that names a module only does something where that module runs. On a media from [method load_from_file], whose input is read through an in-memory access, `:http-referrer=`, `:http-user-agent=` and `:network-caching=` have nothing to affect, while input-level options such as `:start-time=`, `:stop-time=` and `:sub-file=` do. Two names are traps rather than options: `:input-repeat=` has no reader for a media player at all -- looping is [method VLCMediaPlayer.set_abloop_time] -- and `imem-*` names belong to the in-memory access behind [method load_from_file], where setting one conflicts with the callbacks that media was built from.
    ///
    /// # Parameters
    /// - [param option] an option, in `"name=value"` form.
    #[func]
    fn add_option(&self, option: GString) {
        let option = cstring_from_gstring(option);
        unsafe { libvlc_media_add_option(self.media_ptr, option.as_ptr()) }
    }

    /// Add an option to the media, with the flags given instead of the pair [method add_option] uses.
    ///
    /// - [constant OPTION_TRUSTED] applies the option even where LibVLC would refuse it as unsafe. Without it such an option is dropped, and the message that says so -- `unsafe option "..." has been ignored for security reasons` -- is the only option-related line this pair of methods can put in the log.
    /// - [constant OPTION_UNIQUE] ignores the option when the same string is already on this media. Without it the same string is added again.
    ///
    /// `flags = 0` is therefore neither: duplicates accumulate, and options VLC does not consider safe are ignored. Everything else -- reading, validation and the moment the option is read -- is [method add_option].
    ///
    /// # Parameters
    /// - [param option] an option, in `"name=value"` form.
    /// - [param flags] [constant OPTION_TRUSTED], [constant OPTION_UNIQUE], both, or `0`.
    #[func]
    fn add_option_flag(&self, option: GString, flags: i32) {
        let option = cstring_from_gstring(option);
        unsafe { libvlc_media_add_option_flag(self.media_ptr, option.as_ptr(), flags as c_uint) }
    }

    /// Add a subtitle to the media.\
    /// This is [method slaves_add] with [constant SLAVE_TYPE_SUBTITLE] and the [member VLCSubtitle.mrl] of a resource, which is the form a subtitle normally arrives in.
    ///
    /// # When it takes effect
    /// A slave, like an option, is read once -- when the input is created -- and that
    /// happens when the media is **assigned to a [VLCMediaPlayer]**, not when
    /// `play()` is called. So this has to be called first:
    /// `VLCMedia.load_from_file(...)`, then `add_subtitle(...)`, then
    /// `player.media = media`. A subtitle added after the assignment is not seen by
    /// the playback that is running or about to start; it is read by the next input,
    /// which the same media gets after a [method VLCMediaPlayer.stop_async].
    /// To add one to the playback that is already running, use
    /// [method VLCMediaPlayer.add_subtitle] instead -- that is the other half of the
    /// same thing, and it is the only one that works mid-playback.
    ///
    /// # What happens afterwards
    /// There is no way to take a loaded subtitle away again: LibVLC has no
    /// remove-a-slave call, [method slaves_clear] only empties the media's list, and
    /// an empty track selection only unselects -- the track stays. Replacing a
    /// subtitle therefore means adding another one, or stopping and starting the
    /// input over.
    ///
    /// LibVLC rewrites the media's slave list when the input is created, keeping only
    /// the slaves that loaded: one whose URI cannot be opened disappears from the
    /// list for good, with one line in the log about the MRL it could not open.
    ///
    /// # Parameters
    /// - [param subtitle] the subtitle to attach.
    /// - [param priority] from `0` (low) to `4` (high). It decides which slave wins
    ///   when several are attached, and `4` is also what a caller with no opinion
    ///   should pass, since LibVLC folds it and everything above it into its
    ///   "the user asked for this one" rank.
    ///
    /// # Returns
    /// `0` when LibVLC took it, `-1` when it refused -- never an error code, which is
    /// what this libvlc revision returns here.
    #[func]
    fn add_subtitle(&self, subtitle: Gd<VlcSubtitle>, priority: i32) -> i32 {
        self.slaves_add(
            Self::SLAVE_TYPE_SUBTITLE,
            priority,
            subtitle.bind().get_mrl(),
        )
    }

    /// Add an external source to the media descriptor.
    ///
    /// This is `libvlc_media_slaves_add`: a slave is either a subtitle
    /// ([constant SLAVE_TYPE_SUBTITLE]) or some other source
    /// ([constant SLAVE_TYPE_GENERIC]), and `uri` is a media resource locator --
    /// `data:;base64,...` for bytes the caller holds, `file:///...` for a file on the
    /// host, `http(s)://...` for one on a server. A bare filesystem path is not an
    /// MRL: `D:\sub.srt` would be split at its colon and read as a `D` scheme, so
    /// build the MRL with [method VLCSubtitle.load_from_file] or write `file:///`
    /// yourself.
    ///
    /// For a subtitle prefer [method add_subtitle], which does exactly this with the
    /// subtitle type. This entry point is for the other type, and for callers who
    /// already have an MRL.
    ///
    /// The moment it takes effect, the priority, and what happens to a slave that
    /// fails to load are all [method add_subtitle]'s, which documents them.
    ///
    /// # Parameters
    /// - [param slave_type] [constant SLAVE_TYPE_SUBTITLE] or [constant SLAVE_TYPE_GENERIC].
    /// - [param priority] from `0` (low) to `4` (high).
    /// - [param uri] the slave's media resource locator.
    ///
    /// # Returns
    /// `0` when LibVLC took it, `-1` when it refused.
    #[func]
    fn slaves_add(&self, slave_type: i32, priority: i32, uri: GString) -> i32 {
        let uri = cstring_from_gstring(uri);
        unsafe {
            libvlc_media_slaves_add(
                self.media_ptr,
                slave_type as libvlc_media_slave_type_t,
                priority as c_uint,
                uri.as_ptr(),
            )
        }
    }

    /// Drop every slave the media carries.
    ///
    /// This empties the media's own list -- the one the next input will read -- and
    /// nothing else. A playback that is already running keeps the subtitles it
    /// loaded, since those live on the input; to be rid of one there, stop the player
    /// and start it again.
    #[func]
    fn slaves_clear(&self) {
        unsafe { libvlc_media_slaves_clear(self.media_ptr) }
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
    /// the media's meta, or `""` -- which is also what an unparsed media answers,
    /// and what libvlc answers for a media that has no such meta. libvlc hands back
    /// no pointer in that case, so the empty string is the whole of the answer.
    #[func]
    fn get_meta(&self, meta: u32) -> GString {
        let value = unsafe { libvlc_media_get_meta(self.media_ptr, meta as libvlc_meta_t) };
        GString::from(crate::vlc_track::c_string(value).as_str())
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
    fn get_stats(&self) -> VarDictionary {
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
            let mut dict = VarDictionary::new();
            dict.set("read_bytes", stats.i_read_bytes as i64);
            dict.set("input_bitrate", stats.f_input_bitrate);
            dict.set("demux_read_bytes", stats.i_demux_read_bytes as i64);
            dict.set("demux_bitrate", stats.f_demux_bitrate);
            dict.set("demux_corrupted", stats.i_demux_corrupted as i64);
            dict.set("demux_discontinuity", stats.i_demux_discontinuity as i64);
            dict.set("decoded_video", stats.i_decoded_video as i64);
            dict.set("decoded_audio", stats.i_decoded_audio as i64);
            dict.set("displayed_pictures", stats.i_displayed_pictures as i64);
            dict.set("late_pictures", stats.i_late_pictures as i64);
            dict.set("lost_pictures", stats.i_lost_pictures as i64);
            dict.set("played_abuffers", stats.i_played_abuffers as i64);
            dict.set("lost_abuffers", stats.i_lost_abuffers as i64);
            dict
        } else {
            VarDictionary::default()
        }
    }

    /// Get the track list for one type.
    ///
    /// # Note
    /// You need to call [method parse_request] or play the media at least once before calling this function. Not doing this will result in an empty list.
    ///
    /// # Parameters
    /// - [param track_type] type of the track list to request (e.g. [constant VLCTrack.TYPE_VIDEO], [constant VLCTrack.TYPE_AUDIO], [constant VLCTrack.TYPE_TEXT])
    ///
    /// # Returns
    /// a valid [VLCTrackList] or null in case of error, if there is no track for a category, the returned list will have a size of 0.
    ///
    /// # Note
    /// The tracks in it cannot be selected: they come from the media descriptor,
    /// which carries no `es_id`, and libvlc's selection calls act on that. Use
    /// [method VLCMediaPlayer.get_tracklist] once the media is playing, and put
    /// these tracks to work by their [method VLCTrack.get_id] instead.
    #[func]
    fn get_tracklist(&self, track_type: i32) -> Option<Gd<VlcTrackList>> {
        unsafe {
            VlcTrackList::from_ptr(
                libvlc_media_get_tracklist(self.media_ptr, track_type),
                false,
            )
        }
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
        clear_last_error();
        let status = unsafe {
            libvlc_media_parse_request(
                vlc_instance::get(),
                self.media_ptr,
                parse_flag as libvlc_media_parse_flag_t
                    | libvlc_media_parse_flag_t_libvlc_media_fetch_local,
                timeout,
            )
        };
        if status != 0 {
            // The parse was refused, and the caller gets a status and no event --
            // the reason is libvlc's and nowhere else.
            godot_error!(
                "godot-vlc: libvlc refused to parse {}: {}",
                self.get_mrl(),
                last_error()
            );
        }
        status
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
                // The event first, and with the same callback and user data it was
                // attached with: libvlc finds the handler by that pair, and this wrapper
                // may be one of several for the same media (a list hands ports of it
                // around), so detaching is what keeps a freed object out of a later
                // callback. The same gap was recorded for the player in the analysis;
                // lists are what made it reachable, because a list keeps media alive past
                // the wrapper a script built.
                if let Some(weak) = self.self_gd.as_deref_mut() {
                    let data = (&mut **weak) as *mut WeakRef as *mut c_void;
                    libvlc_event_detach(
                        libvlc_media_event_manager(self.media_ptr),
                        libvlc_event_e_libvlc_MediaParsedChanged as libvlc_event_type_t,
                        Some(parsed_changed_callback),
                        data,
                    );
                }
                libvlc_media_release(self.media_ptr);
            }
        }
    }
}

/// Reports a media's parsed status to the main thread, where the signal is emitted.
///
/// Called from libvlc's own threads, so it does no more than hand the number over: the
/// status is read here because the media is the one thing that is certainly alive, and
/// the object may not be. `WeakRef::get_ref` answers nothing for a freed object, and that
/// is a state this callback can genuinely be in -- measured, before the detach in `Drop`
/// existed, as an abort inside godot-rust -- so it returns instead of unwrapping.
unsafe extern "C" fn parsed_changed_callback(
    _event: *const libvlc_event_t,
    user_data: *mut c_void,
) {
    unsafe {
        let Some(weak) = (user_data as *mut WeakRef).as_ref() else {
            return;
        };
        let object = weak.get_ref();
        if object.is_nil() {
            return;
        }
        let mut media = object.to::<Gd<VlcMedia>>();
        let status = libvlc_media_get_parsed_status(media.bind().media_ptr);
        media.call_deferred(
            "emit_signal",
            &[
                StringName::from("parsed_changed").to_variant(),
                status.to_variant(),
            ],
        );
    }
}

unsafe extern "C" fn media_open_callback(
    opaque: *mut c_void,
    datap: *mut *mut c_void,
    sizep: *mut u64,
) -> c_int {
    unsafe {
        if let Some(path) = (opaque as *mut GString).as_ref()
            && let Ok(mut file) = GFile::open(path, ModeFlags::READ)
        {
            *sizep = file.length();
            let _ = file.seek(SeekFrom::Start(0));
            *datap = Box::into_raw(Box::new(file)) as *mut c_void;
            return 0;
        }
        // The path is the one thing this callback knows that the log line does not
        // otherwise carry: libvlc reports the failure against `imem://`, which names
        // no file, so without this a broken path is unattributable.
        match (opaque as *mut GString).as_ref() {
            Some(path) => godot_error!("godot-vlc: unable to open media file {path}"),
            None => godot_error!("godot-vlc: unable to open media file"),
        }
        -1
    }
}

unsafe extern "C" fn media_read_callback(
    opaque: *mut c_void,
    buf: *mut c_uchar,
    len: usize,
) -> isize {
    unsafe {
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
}

unsafe extern "C" fn media_seek_callback(opaque: *mut c_void, offset: u64) -> c_int {
    unsafe {
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

unsafe extern "C" fn media_close_cb(opaque: *mut ::std::os::raw::c_void) {
    unsafe {
        if !opaque.is_null() {
            drop(Box::from_raw(opaque as *mut GFile));
        }
    }
}
