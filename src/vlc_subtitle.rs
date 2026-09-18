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

use godot::{
    classes::{FileAccess as GFile, Marshalls, Resource, file_access::ModeFlags},
    prelude::*,
};

/// An external subtitle track, held as the media resource locator LibVLC takes.
///
/// A subtitle reaches LibVLC as a URI and as nothing else: `libvlc_media_slaves_add`,
/// `libvlc_media_player_add_slave` and the `:sub-file=` option all take a string, and
/// there is no call anywhere that accepts bytes. So this resource *is* an MRL; what
/// it adds is a way to build one out of something Godot can read.
///
/// - A file inside the project (`res://`) or in the user directory (`user://`) has no
///   path LibVLC can open. It lives in the PCK, and VLC's access modules only know
///   the host filesystem, so its bytes are read through [FileAccess] and carried in a
///   `data:;base64,` URI instead. That URI is 4/3 the size of the file and LibVLC
///   decodes it into memory, so this form suits the size subtitles are (kilobytes),
///   not the size audio tracks are.
/// - A path on the host filesystem is kept as a `file://` URI: nothing is copied, and
///   it is the only form that can load a VobSub pair, which is two files --
///   `modules/demux/vobsub.c` opens the `.idx` and finds the `.sub` by rewriting the
///   last four characters of the path it was given.
/// - Anything else is taken as the MRL it already is (`http://...`, `file://...`,
///   `data:...`), which is what a CDN-hosted subtitle or one the caller built itself
///   needs.
///
/// Nothing here is LibVLC's: the resource holds a string, so there is no reference to
/// release and no lifetime to respect. Attaching it copies the string into LibVLC's
/// own media item, after which this resource can be dropped.
#[derive(GodotClass)]
#[class(base=Resource, rename=VLCSubtitle, tool)]
pub struct VlcSubtitle {
    base: Base<Resource>,
    /// The media resource locator this subtitle is handed to LibVLC as.
    ///
    /// Stored rather than computed so that an imported subtitle can carry it: a
    /// `.res` with the bytes already encoded is the point of importing one.
    ///
    /// The property is storage-only -- it is what the saved resource keeps, not
    /// something the inspector shows, and GDScript reads the MRL through
    /// [method get_mrl].
    #[var(usage_flags = [STORAGE])]
    mrl_value: GString,
}

#[godot_api]
impl IResource for VlcSubtitle {
    fn init(base: Base<Resource>) -> Self {
        Self {
            base,
            mrl_value: GString::new(),
        }
    }
}

#[godot_api]
impl VlcSubtitle {
    /// The prefix every in-memory subtitle URI carries.
    ///
    /// `;base64` has to be lowercase and has to be the seven bytes immediately before
    /// the comma: the `data` access matches them with `strncmp` and serves anything
    /// else as literal text. Base64 output contains no `%`, `#` or space, which is
    /// what makes it safe here -- an MRL is cut at the first `#`, and the payload is
    /// percent-decoded before it is base64-decoded.
    const DATA_PREFIX: &'static str = "data:;base64,";

    /// Create a subtitle from a media resource locator.
    ///
    /// The MRL is stored exactly as given: nothing is fetched, checked or converted,
    /// and an MRL LibVLC cannot open fails later, in LibVLC's own log, when the
    /// subtitle is attached rather than here.
    ///
    /// # Parameters
    /// - [param mrl] the media resource locator, e.g. `"https://example.com/sub.srt"`.
    #[func]
    pub fn load_from_mrl(mrl: GString) -> Gd<Self> {
        Gd::from_init_fn(|base| Self {
            base,
            mrl_value: mrl,
        })
    }

    /// Create a subtitle from a file.
    ///
    /// `res://` and `user://` are read through [FileAccess] and carried as a
    /// `data:;base64,` MRL, because LibVLC cannot open a path that exists only inside
    /// the project. Everything else is treated as a path on the host filesystem and
    /// becomes a `file://` MRL; such a path has to be absolute, since a relative one
    /// would be resolved against the process's working directory, which a game does
    /// not control.
    ///
    /// The `data:` form is a copy of the whole file inside one string. It is what
    /// makes a subtitle work from inside a PCK at all, but it is not a way to carry
    /// megabytes: use a path, or a `file://` MRL built by [method load_from_mrl], for
    /// anything large.
    ///
    /// # Parameters
    /// - [param path] a `res://` or `user://` path, or an absolute filesystem path.
    ///
    /// # Returns
    /// a new [VLCSubtitle], or `null` when a `res://`/`user://` file cannot be read.
    #[func]
    pub fn load_from_file(path: GString) -> Option<Gd<Self>> {
        let path_str = path.to_string();
        let mrl_value = if path_str.starts_with("res://") || path_str.starts_with("user://") {
            Self::data_mrl(&path)?
        } else {
            Self::file_mrl(&path_str)
        };
        Some(Gd::from_init_fn(|base| Self { base, mrl_value }))
    }

    /// The media resource locator this subtitle is handed to LibVLC as.
    ///
    /// # Returns
    /// the MRL: a `data:;base64,...` one for content that was read through Godot's
    /// filesystem, otherwise the MRL that was given.
    #[func]
    pub fn get_mrl(&self) -> GString {
        self.mrl_value.clone()
    }

    /// Reads a virtual file and encodes it into a `data:` MRL.
    fn data_mrl(path: &GString) -> Option<GString> {
        let mut file = GFile::open(path, ModeFlags::READ)?;
        let length = file.get_length() as i64;
        let bytes = file.get_buffer(length);
        let encoded = Marshalls::singleton().raw_to_base64(&bytes);
        Some(GString::from(
            format!("{}{encoded}", Self::DATA_PREFIX).as_str(),
        ))
    }

    /// Turns a host path into a `file://` MRL.
    ///
    /// Percent-encoding here is not cosmetic, and this is not a URL encoder either:
    /// the result is what LibVLC parses. `input_SplitMRL` cuts an MRL at the first
    /// `#`, and that happens before any decoding, so a `#` left in a path would
    /// truncate it; a `%` that is not followed by two hex digits makes the `data`
    /// access fail outright. Backslashes are separators on the way in, which is why
    /// they are converted first -- they are not escapes in a URI.
    fn file_mrl(path: &str) -> GString {
        let path = path.replace('\\', "/");
        // `//host/share` is a UNC path, and VLC wants its host left out of the path
        // part; a single leading slash is the ordinary absolute path; anything else
        // needs the root added.
        let mut mrl = String::from(if path.starts_with("//") {
            "file:"
        } else if path.starts_with('/') {
            "file://"
        } else {
            "file:///"
        });
        for byte in path.bytes() {
            match byte {
                b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'.'
                | b'_'
                | b'~'
                | b'/'
                | b':' => mrl.push(byte as char),
                _ => mrl.push_str(&format!("%{byte:02X}")),
            }
        }
        GString::from(mrl.as_str())
    }
}
