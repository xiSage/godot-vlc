use godot::{classes::Engine, prelude::*};


#[allow(dead_code, non_camel_case_types, non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms, unused_imports)]
mod vlc { include!(concat!(env!("OUT_DIR"), "/vlc_bindings.rs")); }
mod vlc_instance;
mod vlc_media_player;
mod vlc_media;

struct GodotVLCExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotVLCExtension {
    fn on_level_init(level: InitLevel) {
        if level == InitLevel::Scene {
            Engine::singleton().register_singleton(
                "VLCInstance",
                &vlc_instance::VLCInstance::new_alloc()
            );
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