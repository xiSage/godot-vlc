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

use godot::{classes::Engine, prelude::*};

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
mod vlc_instance;
mod vlc_media;
mod vlc_media_player;
mod vlc_track;
mod vlc_track_list;

struct GodotVLCExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotVLCExtension {
    fn on_level_init(level: InitLevel) {
        if level == InitLevel::Scene {
            Engine::singleton()
                .register_singleton("VLCInstance", &vlc_instance::VLCInstance::new_alloc());
        }
    }

    fn on_level_deinit(level: InitLevel) {
        if level == InitLevel::Scene {
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
