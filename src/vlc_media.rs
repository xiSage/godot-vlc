use std::{
    ffi::{c_int, c_uchar, c_void},
    io::{Read, Seek, SeekFrom},
};

use crate::vlc::*;
use godot::{classes::file_access::ModeFlags, prelude::*};

#[derive(GodotClass)]
#[class(base=Resource, rename=VLCMedia, no_init)]
pub struct VlcMedia {
    base: Base<Resource>,
    // file: *mut GFile,
    path: *mut GString,
    pub media_ptr: *mut libvlc_media_t,
}

#[godot_api]
impl VlcMedia {
    #[func]
    fn load_from_file(path: GString) -> Option<Gd<Self>> {
        let path = Box::into_raw(Box::new(path));
        let media_ptr = unsafe {
            libvlc_media_new_callbacks(
                Some(media_open_callback),
                Some(media_read_callback),
                Some(media_seek_callback),
                Some(media_close_cb),
                path as *mut c_void,
            )
        };
        
        Some(Gd::from_init_fn(|base| Self {
            base,
            path,
            media_ptr,
        }))
    }
}

impl Drop for VlcMedia {
    fn drop(&mut self) {
        unsafe {
            if !self.media_ptr.is_null() {
                libvlc_media_release(self.media_ptr);
            }
            if !self.path.is_null() {
                drop(Box::from_raw(self.path));
            }
        }
    }
}

unsafe extern "C" fn media_open_callback(
    opaque: *mut c_void,
    datap: *mut *mut c_void,
    sizep: *mut u64,
) -> c_int {
    if let Some(path) = (opaque as *mut GString).as_ref() {
        if let Ok(mut file) = GFile::open(path, ModeFlags::READ) {
            *sizep = file.length();
            let _ = file.seek(SeekFrom::Start(0));
            *datap = Box::into_raw(Box::new(file)) as *mut c_void;
            return 0;
        }
    }
    godot_error!("godot-vlc: unable to open media file");
    -1
}

unsafe extern "C" fn media_read_callback(
    opaque: *mut c_void,
    buf: *mut c_uchar,
    len: usize,
) -> isize {
    if let Some(file) = (opaque as *mut GFile).as_mut() {
        let buf = std::slice::from_raw_parts_mut(buf, len);
        match file.read(buf) {
            Ok(n) => n.try_into().unwrap(),
            Err(_) => -1,
        }
    } else {
        -1
    }
}

unsafe extern "C" fn media_seek_callback(opaque: *mut c_void, offset: u64) -> c_int {
    if let Some(file) = (opaque as *mut GFile).as_mut() {
        match file.seek(SeekFrom::Start(offset)) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    } else {
        -1
    }
}

unsafe extern "C" fn media_close_cb(opaque: *mut ::std::os::raw::c_void) {
    if !opaque.is_null() {
        drop(Box::from_raw(opaque as *mut GFile));
    }
}
