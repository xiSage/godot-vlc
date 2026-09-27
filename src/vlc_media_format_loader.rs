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

use crate::vlc_media::VlcMedia;
use godot::{
    classes::{
        FileAccess as GFile, IResourceFormatLoader, ProjectSettings, ResourceFormatLoader,
        file_access::ModeFlags,
    },
    global::Error,
    prelude::*,
};

/// The project setting that holds the extensions this loader answers for.
///
/// It is a [PackedStringArray] holding the whole list rather than a set of additions to
/// one, so a project can drop an extension VLC offers as easily as it can add one VLC
/// does not. The entries are read as they are written -- `".FLAC"`, `"flac"` and
/// `" flac "` all mean the same extension -- and an empty array means this loader
/// answers for nothing at all.
///
/// A change is visible to a running project: the engine asks a loader what it
/// recognizes every time it resolves a path, so a `load()` or an `exists()` after the
/// change already sees it. The editor's file system panel keeps its own copy of the
/// list between scans, so a rescan is what makes the panel show the new extensions too.
const EXTENSIONS_SETTING: &str = "vlc/media_extensions";

/// The extensions VLC itself offers for local files, as the pinned revision declares
/// them.
///
/// This is the pair of globs `EXTENSIONS_AUDIO` and `EXTENSIONS_VIDEO` in VLC's
/// `include/vlc_interface.h` -- the list its own open dialogs are built from --
/// concatenated and de-duplicated, in the order they appear there. It is not read from
/// VLC at runtime, and cannot be: the shortcuts a module declares live in `module_t`,
/// which the public headers keep opaque, and a resource loader has to answer before any
/// file is opened. `scripts/check_media_extensions.ps1` reads that header from the
/// revision `build/vlc/vlc.lock` pins and reports any difference, so the list can be
/// re-derived rather than trusted; this is what the setting [EXTENSIONS_SETTING] starts
/// from.
///
/// The playlist globs of that header are left out on purpose: a playlist is a list of
/// media rather than one media, and this loader hands one file to one [VLCMedia].
const DEFAULT_EXTENSIONS: &[&str] = &[
    "3ga", "669", "a52", "aac", "ac3", "adt", "adts", "aif", "aifc", "aiff", "alac", "amb", "amr",
    "aob", "ape", "au", "awb", "caf", "dts", "dsf", "dff", "flac", "it", "kar", "m4a", "m4b",
    "m4p", "m5p", "mid", "mka", "mlp", "mod", "mpa", "mp1", "mp2", "mp3", "mpc", "mpga", "mus",
    "oga", "ogg", "oma", "opus", "qcp", "ra", "rmi", "s3m", "sid", "spx", "tak", "thd", "tta",
    "voc", "vqf", "w64", "wav", "wma", "wv", "xa", "xm", "3g2", "3gp", "3gp2", "3gpp", "amrec",
    "amv", "asf", "avi", "bik", "bin", "crf", "dav", "divx", "drc", "dv", "dvr-ms", "evo", "f4v",
    "flv", "gvi", "gxf", "iso", "k3g", "m1v", "m2v", "m2t", "m2ts", "m4v", "mkv", "mov", "mp2v",
    "mp4", "mp4v", "mpe", "mpeg", "mpeg1", "mpeg2", "mpeg4", "mpg", "mpv2", "mts", "mtv", "mxf",
    "mxg", "nsv", "nuv", "ogm", "ogv", "ogx", "ps", "qt", "rec", "rm", "rmvb", "rpl", "skm", "thp",
    "tod", "tp", "ts", "tts", "txd", "vob", "vp6", "vro", "webm", "wm", "wmv", "wtv", "xesc",
];

/// Registers [EXTENSIONS_SETTING], with [DEFAULT_EXTENSIONS] as its default.
///
/// This runs once, while the extension is being loaded and before the loader itself is
/// registered: the engine drops a property info block whose setting does not exist yet,
/// and the loader reads the setting from its first call onwards.
pub(crate) fn register_extensions_setting() {
    let mut settings = ProjectSettings::singleton();
    let default = default_extensions();
    if !settings.has_setting(EXTENSIONS_SETTING) {
        settings.set_setting(EXTENSIONS_SETTING, &default.to_variant());
    }
    settings.set_initial_value(EXTENSIONS_SETTING, &default.to_variant());
    let mut info = VarDictionary::new();
    let _ = info.insert("name", EXTENSIONS_SETTING);
    let _ = info.insert("type", VariantType::PACKED_STRING_ARRAY);
    settings.add_property_info(&info);
    // Deliberately not `set_restart_if_changed`, which the two settings `VLCInstance`
    // registers do use: an instance option has already been handed to libvlc by the time
    // it changes, while this list is read again on every path the loader resolves, so a
    // restart would only make the project wait for something that already works.
}

/// [DEFAULT_EXTENSIONS] as the array a setting holds.
fn default_extensions() -> PackedStringArray {
    DEFAULT_EXTENSIONS
        .iter()
        .map(|e| GString::from(*e))
        .collect()
}

/// The extensions to answer for, taken from [EXTENSIONS_SETTING].
fn media_extensions() -> Vec<GString> {
    let configured = ProjectSettings::singleton().get_setting(EXTENSIONS_SETTING);
    if let Ok(extensions) = configured.try_to::<PackedStringArray>() {
        return normalize(extensions.as_slice().iter().cloned());
    }
    // A project that wrote the setting by hand may have used a plain array where the
    // editor writes a packed one. Anything that is not a list of strings at all falls
    // back to the default rather than to no extensions: a setting this loader cannot
    // read should not take its whole list away.
    if let Ok(extensions) = configured.try_to::<Array<GString>>() {
        return normalize(extensions.iter_shared());
    }
    normalize(default_extensions().as_slice().iter().cloned())
}

/// The extensions as a loader has to report them: without surrounding space, without a
/// leading dot, in lower case, without empties, and each one only once.
///
/// Godot asks for bare lower-case extensions and matches them against what
/// `String.get_extension()` returns -- the part of a path after its last dot -- so
/// `".FLAC"` and `"flac"` are one entry and `""` is not an entry at all.
fn normalize(extensions: impl IntoIterator<Item = GString>) -> Vec<GString> {
    let mut normalized: Vec<GString> = Vec::new();
    for extension in extensions {
        let extension = extension
            .to_string()
            .trim()
            .trim_start_matches('.')
            .to_lowercase();
        if extension.is_empty() {
            continue;
        }
        let extension = GString::from(extension.as_str());
        if !normalized.contains(&extension) {
            normalized.push(extension);
        }
    }
    normalized
}

/// Loads the media files inside a project as [VLCMedia] resources.\
/// This is what makes `res://movie.mp4` something a scene can reference and a script can `load()`, and it is the reason a media file does not have to be named by an absolute path. Which files those are is the project setting [EXTENSIONS_SETTING], which starts from VLC's own list and can be added to or cut down per project.
///
/// # Why this is not a GDScript
/// It used to be, and the script reached the class through the `VLCInstance` engine
/// singleton rather than calling `VLCMedia.load_from_file` directly. The reason was
/// real: a script that names a GDExtension class is parsed before the extension is
/// registered -- on a project's first scan, every script is parsed before the
/// extensions the scan discovers are loaded -- so the loader could not mention
/// `VLCMedia` without the parse failing. A Rust loader has no parse step, so that
/// constraint is gone, and the class is registered by the extension itself...
///
/// # ...which is what registering means here
/// Godot does not go looking for `ResourceFormatLoader` subclasses. It keeps a list of
/// loader instances, filled by [method ResourceLoader.add_resource_format_loader], and
/// the loader that used to do this job was found through another mechanism entirely: a
/// script with a global class name is registered by `ScriptServer`, which no native
/// class can be part of. So [crate::GodotVLCExtension] registers this one at
/// [enum InitStage.SCENE] -- the stage before the engine's own custom-loader pass and
/// before any project file is loaded -- and unregisters it when the extension is
/// unloaded, since nothing else would.
#[derive(GodotClass)]
#[class(tool, init, base=ResourceFormatLoader)]
pub struct VlcMediaFormatLoader {
    base: Base<ResourceFormatLoader>,
}

#[godot_api]
impl IResourceFormatLoader for VlcMediaFormatLoader {
    fn get_recognized_extensions(&self) -> PackedStringArray {
        media_extensions().into_iter().collect()
    }

    /// Which resource type a path would load as, or `""` for a path this loader does
    /// not handle.
    fn get_resource_type(&self, path: GString) -> GString {
        let extension = GString::from(path.get_extension().to_string().to_lowercase().as_str());
        if media_extensions().contains(&extension) {
            GString::from("VLCMedia")
        } else {
            GString::new()
        }
    }

    /// The script this replaced answered `true` for every type, which only widened
    /// where its extensions were considered. A native class can answer the question
    /// the engine is actually asking, and a scene's `[ext_resource type="VLCMedia"]`
    /// is the case that matters: the hint is matched here before the extension is.
    fn handles_type(&self, type_name: StringName) -> bool {
        type_name == "VLCMedia"
    }

    /// Loads a media file as a resource.
    ///
    /// The file is opened first so that one which cannot be read is reported as such,
    /// rather than becoming a media that fails later with nothing pointing at the
    /// path. Everything after that is [method VLCMedia.load_from_file], which hands
    /// the file to LibVLC when the operating system can see it and reads it through
    /// Godot's own filesystem otherwise -- the second one is what keeps this working
    /// inside an exported project, where the media lives in the PCK and has no path
    /// LibVLC could open.
    fn load(
        &self,
        path: GString,
        _original_path: GString,
        _use_sub_threads: bool,
        _cache_mode: i32,
    ) -> Variant {
        if GFile::open(&path, ModeFlags::READ).is_none() {
            return Error::ERR_CANT_OPEN.to_variant();
        }
        VlcMedia::load_from_file(path).to_variant()
    }
}
