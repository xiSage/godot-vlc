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

use crate::{util::cstring_from_gstring, vlc};
use godot::{
    classes::{
        Engine, ProjectSettings, class_macros::sys::GDEXTENSION_VARIANT_TYPE_STRING,
        notify::ObjectNotification,
    },
    prelude::*,
    register::info::PropertyHint,
};
use printf::printf;
use std::{
    ffi::{CString, c_char, c_int, c_void},
    mem,
};

pub fn get() -> *mut vlc::libvlc_instance_t {
    Engine::singleton()
        .get_singleton("VLCInstance")
        .expect("VLCInstance not found")
        .cast::<VLCInstance>()
        .bind()
        .get_vlc_instance()
}

#[derive(GodotClass)]
#[class(base=Object, tool)]
pub struct VLCInstance {
    instance: Option<*mut vlc::libvlc_instance_t>,
    base: Base<Object>,
}

#[godot_api]
impl IObject for VLCInstance {
    fn init(base: Base<Object>) -> Self {
        if !ProjectSettings::singleton().has_setting("vlc/log_level") {
            ProjectSettings::singleton().set_setting("vlc/log_level", &Variant::from(4));
        }
        ProjectSettings::singleton().set_initial_value("vlc/log_level", &Variant::from(4));
        let mut info = VarDictionary::new();
        let _ = info.insert("name", "vlc/log_level");
        let _ = info.insert("type", VariantType::INT);
        let _ = info.insert("hint", PropertyHint::ENUM);
        let _ = info.insert("hint_string", "Debug, Info, Warning, Error, Disabled");
        ProjectSettings::singleton().add_property_info(&info);
        ProjectSettings::singleton().set_restart_if_changed("vlc/log_level", true);
        let debug_level: i32 = ProjectSettings::singleton()
            .get_setting("vlc/log_level")
            .try_to()
            .unwrap();

        if !ProjectSettings::singleton().has_setting("vlc/arguments") {
            ProjectSettings::singleton()
                .set_setting("vlc/arguments", &Variant::from(Array::<GString>::new()));
        }
        ProjectSettings::singleton()
            .set_initial_value("vlc/arguments", &Variant::from(Array::<GString>::new()));
        let mut info = VarDictionary::new();
        let _ = info.insert("name", "vlc/arguments");
        let _ = info.insert("type", VariantType::ARRAY);
        let _ = info.insert("hint", PropertyHint::TYPE_STRING);
        let _ = info.insert(
            "hint_string",
            format!("{}:", GDEXTENSION_VARIANT_TYPE_STRING),
        );
        ProjectSettings::singleton().add_property_info(&info);
        ProjectSettings::singleton().set_restart_if_changed("vlc/arguments", true);
        let configured_arguments: Array<GString> = ProjectSettings::singleton()
            .get_setting("vlc/arguments")
            .try_to()
            .unwrap_or_default();

        // Copied rather than used in place: a Godot Array is reference-counted,
        // so appending the Android default to it would edit the project setting.
        #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
        let mut arguments: Vec<GString> = configured_arguments.iter_shared().collect();

        // Android has no window for LibVLC to draw into, and the software
        // callbacks are the only output that can produce a texture, so `vmem` is
        // what this extension always wants there. Registering those callbacks
        // already sets the media player's own `vout`, which outranks an instance
        // option; this default earns its place when that registration did not
        // happen, where it turns a silent "no video" into vmem's own "missing
        // lock callback" error -- the difference between a symptom and a reason.
        #[cfg(target_os = "android")]
        {
            let already_chosen = arguments.iter().any(|argument| {
                let argument = argument.to_string();
                argument.starts_with("--vout") || argument.starts_with(":vout")
            });
            if !already_chosen {
                arguments.push(GString::from("--vout=vmem"));
            }
        }

        let args: Vec<CString> = arguments.into_iter().map(cstring_from_gstring).collect();
        let argc = args.len() as c_int;
        let args: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let argv = args.as_ptr();

        // LibVLC derives its plugin, libexec and data directories from the
        // location of its own module and says nothing when they turn out not to
        // exist. Point them explicitly at the runtime we ship before any
        // instance is created. See src/vlc_runtime.rs.
        crate::vlc_runtime::configure_vlc_paths();

        let instance = unsafe { vlc::libvlc_new(argc, argv) };
        #[allow(clippy::missing_transmute_annotations)]
        let cb = unsafe { Some(mem::transmute(VLCInstance::log_callback_impl as *const ())) };
        unsafe {
            vlc::libvlc_log_set(instance, cb, debug_level as *mut c_void);
        }
        let instance = Some(instance);
        Self { instance, base }
    }

    fn on_notification(&mut self, what: ObjectNotification) {
        if what == ObjectNotification::PREDELETE && self.instance.is_some() {
            unsafe {
                vlc::libvlc_release(self.instance.unwrap());
                self.instance = None;
            }
        }
    }
}

#[godot_api]
impl VLCInstance {
    pub fn get_vlc_instance(&self) -> *mut vlc::libvlc_instance_t {
        self.instance.unwrap()
    }

    unsafe extern "C" fn log_callback_impl(
        _data: *mut c_void,
        level: c_int,
        _ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: *mut c_void,
    ) {
        unsafe {
            let min_level = _data as i32;
            let s: String = printf(fmt, args);
            match level as vlc::libvlc_log_level {
                vlc::libvlc_log_level_LIBVLC_DEBUG if min_level <= 0 => {
                    godot_print!("LibVLC: [DEBUG] {}", s);
                }
                vlc::libvlc_log_level_LIBVLC_NOTICE if min_level <= 1 => {
                    godot_print!("LibVLC: {}", s);
                }
                vlc::libvlc_log_level_LIBVLC_WARNING if min_level <= 2 => {
                    godot_warn!("LibVLC: {}", s);
                }
                vlc::libvlc_log_level_LIBVLC_ERROR if min_level <= 3 => {
                    godot_error!("LibVLC: {}", s);
                }
                _ => {}
            }
        }
    }
}
