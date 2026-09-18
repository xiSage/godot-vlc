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

use crate::vlc_subtitle_importer::VlcSubtitleImporter;
use godot::{
    classes::{EditorImportPlugin, EditorPlugin, IEditorPlugin},
    prelude::*,
};

/// The addon's editor half, which exists to register the subtitle importer.
///
/// A GDExtension's `EditorPlugin` needs no `plugin.cfg` and no entry in the project's
/// enabled-plugin list: the editor instantiates every `EditorPlugin` class an
/// extension registers, at the editor stage of startup, and calls `_enter_tree` on it
/// before the project's files are first scanned. That is what lets the importer be
/// registered in the right order without anything to switch on in Project Settings.
///
/// It also means there is nothing to switch **off**. The subtitle importer is active
/// in every project that has this addon, in the editor, for every recognised
/// extension; the per-file escape is Godot's own "Keep File" import mode, which turns
/// the import into a plain copy of the source.
///
/// The class is a `tool` class deliberately: a non-tool class of an extension is a
/// placeholder in the editor, with no Rust instance behind it, and `_enter_tree`
/// would never reach this code.
#[derive(GodotClass)]
#[class(tool, init, base=EditorPlugin)]
pub struct VlcEditorPlugin {
    base: Base<EditorPlugin>,
    /// Held for as long as the plugin lives: `add_import_plugin` keeps a reference to
    /// what it was given, and `remove_import_plugin` has to be handed the same object.
    importer: Option<Gd<VlcSubtitleImporter>>,
}

#[godot_api]
impl IEditorPlugin for VlcEditorPlugin {
    fn enter_tree(&mut self) {
        let importer = VlcSubtitleImporter::new_gd();
        let as_editor_import_plugin: Gd<EditorImportPlugin> = importer.clone().upcast();
        self.base_mut().add_import_plugin(&as_editor_import_plugin);
        self.importer = Some(importer);
    }

    fn exit_tree(&mut self) {
        if let Some(importer) = self.importer.take() {
            let as_editor_import_plugin: Gd<EditorImportPlugin> = importer.upcast();
            self.base_mut()
                .remove_import_plugin(&as_editor_import_plugin);
        }
    }
}
