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
        FileAccess as GFile, IResourceFormatLoader, ResourceFormatLoader, file_access::ModeFlags,
    },
    global::Error,
    prelude::*,
};

/// The extensions that become [VLCMedia] resources.
///
/// This is VLC's own list of what it can open, with the containers it demuxes; it is
/// not derived from anything at runtime, because a resource loader has to answer
/// before a file is opened. One entry is deliberately corrected from the script this
/// replaced: `.xesc` carried a leading dot, which `String.get_extension()` never
/// returns, so that file was the one extension of the list that could not load.
const MEDIA_EXTENSIONS: &[&str] = &[
    // audio
    "3ga", "669", "a52", "acc", "ac3", "adt", "adts", "aif", "aifc", "aiff", "alac", "amr", "aob",
    "au", "ape", "caf", "cda", "dts", "dsf", "dff", "flac", "it", "m4a", "m4p", "mka", "mlp",
    "mod", "mp1", "mp2", "mp3", "mpc", "mpga", "oga", "oma", "opus", "qcp", "ra", "rmi", "snd",
    "s3m", "spx", "tak", "tta", "voc", "vqf", "w64", "wav", "wma", "wv", "xa", "xm",
    // video
    "3g2", "3gp", "3gp2", "3gpp", "amrec", "amv", "asf", "avi", "bik", "dav", "divx", "drc", "dv",
    "dvr-ms", "evo", "f4v", "flv", "gvi", "gxf", "k3g", "m1v", "m2t", "m2v", "m2ts", "m4v", "mkv",
    "mov", "mp2v", "mp4", "mp4v", "mpa", "mpe", "mpeg", "mpeg1", "mpeg2", "mpeg4", "mpg", "mpv2",
    "mts", "mtv", "mxf", "nsv", "nuv", "ogg", "ogm", "ogx", "ogv", "qt", "rec", "rm", "rmvb",
    "rpl", "skm", "thp", "tod", "tp", "ts", "tts", "vob", "vp6", "vro", "webm", "wmv", "wtv",
    "xesc",
];

/// Loads the media files inside a project as [VLCMedia] resources.\
/// This is what makes `res://movie.mp4` something a scene can reference and a script can `load()`, and it is the reason a media file does not have to be named by an absolute path.
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
        let mut extensions = PackedStringArray::new();
        for extension in MEDIA_EXTENSIONS {
            extensions.push(&GString::from(*extension));
        }
        extensions
    }

    /// Which resource type a path would load as, or `""` for a path this loader does
    /// not handle.
    fn get_resource_type(&self, path: GString) -> GString {
        let extension = path.get_extension().to_string();
        if MEDIA_EXTENSIONS.contains(&extension.as_str()) {
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
    /// path. Everything after that is [method VLCMedia.load_from_file], which reads
    /// the file through Godot's own filesystem -- which is what makes this work inside
    /// an exported project, where the media lives in the PCK and has no path LibVLC
    /// could open.
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
