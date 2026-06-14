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
        let arguments: Array<GString> = ProjectSettings::singleton()
            .get_setting("vlc/arguments")
            .try_to()
            .unwrap_or_default();
        let args: Vec<CString> = arguments.iter_shared().map(cstring_from_gstring).collect();
        let argc = args.len() as c_int;
        let args: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let argv = args.as_ptr();

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
