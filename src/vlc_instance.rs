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

use crate::vlc;
use godot::{
    classes::{notify::ObjectNotification, Engine, ProjectSettings},
    global::PropertyHint,
    prelude::*,
};
use printf::printf;
use std::{
    ffi::{c_char, c_int, c_void},
    ptr,
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
            ProjectSettings::singleton().set_setting("vlc/log_level", &Variant::from(3));
        }
        ProjectSettings::singleton().set_initial_value("vlc/log_level", &Variant::from(3));
        let mut info = Dictionary::new();
        let _ = info.insert("name", "vlc/log_level");
        let _ = info.insert("type", VariantType::INT);
        let _ = info.insert("hint", PropertyHint::ENUM);
        let _ = info.insert("hint_string", "Debug, Info, Warning, Error, Disabled");
        ProjectSettings::singleton().add_property_info(&info);
        let debug_level: i32 = ProjectSettings::singleton()
            .get_setting("vlc/log_level")
            .try_to()
            .unwrap();
        let instance = unsafe { vlc::libvlc_new(0, ptr::null()) };
        unsafe {
            match debug_level {
                0 => vlc::libvlc_log_set(
                    instance,
                    Some(VLCInstance::log_callback_debug),
                    ptr::null_mut(),
                ),
                1 => vlc::libvlc_log_set(
                    instance,
                    Some(VLCInstance::log_callback_info),
                    ptr::null_mut(),
                ),
                2 => vlc::libvlc_log_set(
                    instance,
                    Some(VLCInstance::log_callback_warning),
                    ptr::null_mut(),
                ),
                3 => vlc::libvlc_log_set(
                    instance,
                    Some(VLCInstance::log_callback_error),
                    ptr::null_mut(),
                ),
                _ => {}
            }
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

    unsafe extern "C" fn log_callback_debug(
        _data: *mut c_void,
        level: c_int,
        _ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: vlc::va_list,
    ) {
        let s: String = printf(fmt, args as *mut _);
        match level {
            vlc::libvlc_log_level_LIBVLC_NOTICE => {
                godot_print!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_WARNING => {
                godot_warn!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_ERROR => {
                godot_error!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_DEBUG => {
                godot_print!("LibVLC: [DEBUG] {}", s);
            }
            _ => {}
        }
    }

    unsafe extern "C" fn log_callback_info(
        _data: *mut c_void,
        level: c_int,
        _ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: vlc::va_list,
    ) {
        let s: String = printf(fmt, args as *mut _);
        match level {
            vlc::libvlc_log_level_LIBVLC_NOTICE => {
                godot_print!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_WARNING => {
                godot_warn!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_ERROR => {
                godot_error!("LibVLC: {}", s);
            }
            _ => {}
        }
    }

    unsafe extern "C" fn log_callback_warning(
        _data: *mut c_void,
        level: c_int,
        _ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: vlc::va_list,
    ) {
        let s: String = printf(fmt, args as *mut _);
        match level {
            vlc::libvlc_log_level_LIBVLC_WARNING => {
                godot_warn!("LibVLC: {}", s);
            }
            vlc::libvlc_log_level_LIBVLC_ERROR => {
                godot_error!("LibVLC: {}", s);
            }
            _ => {}
        }
    }

    unsafe extern "C" fn log_callback_error(
        _data: *mut c_void,
        level: c_int,
        _ctx: *const vlc::libvlc_log_t,
        fmt: *const c_char,
        args: vlc::va_list,
    ) {
        let s: String = printf(fmt, args as *mut _);
        if level == vlc::libvlc_log_level_LIBVLC_ERROR {
            godot_error!("LibVLC: {}", s);
        }
    }
}
