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

use std::cell::RefCell;

use godot::{
    classes::{Engine, ResourceLoader},
    prelude::*,
};

mod util;
#[allow(
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    clippy::upper_case_acronyms,
    unused_imports
)]
mod vlc {
    include!(concat!(env!("OUT_DIR"), "/vlc_bindings.rs"));
}
#[cfg(test)]
mod acceptance;
mod vlc_editor_plugin;
mod vlc_instance;
mod vlc_media;
mod vlc_media_format_loader;
mod vlc_media_player;
mod vlc_runtime;
mod vlc_subtitle;
mod vlc_subtitle_importer;
mod vlc_track;
mod vlc_track_list;

thread_local! {
    /// The media resource loader, held for as long as the extension is loaded.
    ///
    /// It cannot be a plain `static`: `Gd` is neither `Send` nor `Sync`, and both ends
    /// of its life -- registering it and unregistering it -- happen on the main
    /// thread. It has to be held at all because `remove_resource_format_loader` wants
    /// the same object back: the engine's loader table holds a reference of its own,
    /// but offers no way to ask it for the loader.
    static MEDIA_FORMAT_LOADER: RefCell<Option<Gd<vlc_media_format_loader::VlcMediaFormatLoader>>> =
        const { RefCell::new(None) };
}

struct GodotVLCExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotVLCExtension {
    fn on_stage_init(stage: InitStage) {
        if stage == InitStage::Scene {
            Engine::singleton()
                .register_singleton("VLCInstance", &vlc_instance::VLCInstance::new_alloc());

            // Godot does not find resource loaders by looking for loader classes: the
            // table it consults is filled by `add_resource_format_loader` alone, and
            // the script this replaced got in through `ScriptServer`'s global-class
            // list, which no native class can be part of. `Scene` runs in exported
            // games -- where `Editor` never does -- and before the engine's own
            // custom-loader pass, so it is the stage this belongs to.
            MEDIA_FORMAT_LOADER.with(|loader| {
                let mut loader = loader.borrow_mut();
                if loader.is_none() {
                    let instance = vlc_media_format_loader::VlcMediaFormatLoader::new_gd();
                    ResourceLoader::singleton().add_resource_format_loader(&instance);
                    *loader = Some(instance);
                }
            });
        }
    }

    fn on_stage_deinit(stage: InitStage) {
        if stage == InitStage::Scene {
            // Nothing removes a loader an extension registered, so an editor that
            // reloads this library would leave the table pointing into it.
            MEDIA_FORMAT_LOADER.with(|loader| {
                if let Some(instance) = loader.borrow_mut().take() {
                    ResourceLoader::singleton().remove_resource_format_loader(&instance);
                }
            });

            let mut engine = Engine::singleton();
            let singleton_name = "VLCInstance";

            if let Some(singleton) = engine.get_singleton(singleton_name) {
                engine.unregister_singleton(singleton_name);
                singleton.free();
            } else {
                godot_error!("Singleton not found: {singleton_name}")
            }
        }
    }
}
