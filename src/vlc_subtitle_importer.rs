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

use crate::vlc_subtitle::VlcSubtitle;
use godot::{
    classes::{EditorImportPlugin, IEditorImportPlugin, ResourceSaver},
    global::Error,
    prelude::*,
};

/// The extensions this importer claims.
///
/// `idx` is deliberately absent, and so is the VobSub half of `sub`: a VobSub subtitle
/// is a pair of files, and this route carries one file's bytes. Its demux finds the
/// second file by rewriting the path it was handed, which a `data:` URI cannot
/// survive -- see [crate::vlc_subtitle]. A `.sub` that turns out to be that binary
/// format is refused when it is read, with a message saying what to use instead.
const SUBTITLE_EXTENSIONS: [&str; 7] = ["srt", "ass", "ssa", "vtt", "smi", "sub", "ttml"];

/// Imports a subtitle file into a [VLCSubtitle] whose MRL carries its bytes.
///
/// Godot calls this when it scans the project, and the resource it saves is the whole
/// point: `res://sub.srt` becomes a resource holding a `data:;base64,` MRL, so the
/// exported project needs neither the original file nor any filesystem path for it.
/// That is what LibVLC needs and cannot have otherwise -- its access modules cannot
/// open anything inside a PCK, and a subtitle reaches it as a URI and nothing else.
///
/// This class does not register itself. Nothing in Godot goes looking for an
/// `EditorImportPlugin`; the editor learns about one from
/// [method EditorPlugin.add_import_plugin], which
/// [method VlcEditorPlugin._enter_tree] calls.
#[derive(GodotClass)]
#[class(tool, init, base=EditorImportPlugin)]
pub struct VlcSubtitleImporter {
    base: Base<EditorImportPlugin>,
}

#[godot_api]
impl IEditorImportPlugin for VlcSubtitleImporter {
    fn get_importer_name(&self) -> GString {
        GString::from("godot_vlc.subtitle")
    }

    fn get_visible_name(&self) -> GString {
        GString::from("VLC subtitle")
    }

    fn get_recognized_extensions(&self) -> PackedStringArray {
        let mut extensions = PackedStringArray::new();
        for extension in SUBTITLE_EXTENSIONS {
            extensions.push(&GString::from(extension));
        }
        extensions
    }

    fn get_save_extension(&self) -> GString {
        GString::from("res")
    }

    fn get_resource_type(&self) -> GString {
        GString::from("VLCSubtitle")
    }

    fn get_preset_count(&self) -> i32 {
        1
    }

    fn get_preset_name(&self, _preset_index: i32) -> GString {
        GString::from("Default")
    }

    /// No options: the one decision this importer makes is not a user's to make.
    ///
    /// A subtitle inside the project has to be carried in its own URI, because there
    /// is no path for it that LibVLC could open, and a subtitle that already has a
    /// path on the host filesystem does not belong in the import pipeline at all --
    /// its MRL is the path, and [method VLCSubtitle.load_from_file] builds one.
    fn get_import_options(&self, _path: GString, _preset_index: i32) -> Array<AnyDictionary> {
        Array::new()
    }

    /// Reads the source through Godot's filesystem and saves the MRL it becomes.
    fn import(
        &mut self,
        source_file: GString,
        save_path: GString,
        _options: VarDictionary,
        _platform_variants: Array<GString>,
        _gen_files: Array<GString>,
    ) -> Error {
        // The same call a script would make, so the two routes cannot disagree about
        // what a file becomes: `load_from_file` reads the bytes and says why when it
        // cannot.
        let Some(subtitle) = VlcSubtitle::load_from_file(source_file.clone()) else {
            godot_error!("godot-vlc: {source_file} could not be imported as a subtitle");
            return Error::FAILED;
        };

        let path = GString::from(format!("{save_path}.{}", self.get_save_extension()).as_str());
        let resource: Gd<Resource> = subtitle.upcast();
        let error = ResourceSaver::singleton()
            .save_ex(&resource)
            .path(&path)
            .done();
        if error != Error::OK {
            godot_error!("godot-vlc: the imported subtitle could not be saved to {path}");
        }
        error
    }
}
