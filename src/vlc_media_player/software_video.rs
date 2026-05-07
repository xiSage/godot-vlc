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

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr,
    sync::mpsc,
};

use godot::{
    classes::{image, Image},
    prelude::*,
};

pub(super) unsafe extern "C" fn video_lock_callback(
    opaque: *mut c_void,
    planes: *mut *mut c_void,
) -> *mut c_void {
    let (_tx, _img, buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    let buffer_ptr = buffer.as_mut_slice().as_mut_ptr();
    *planes = buffer_ptr as *mut c_void;
    ptr::null_mut()
}

pub(super) unsafe extern "C" fn video_unlock_callback(
    opaque: *mut c_void,
    _picture: *mut c_void,
    _planes: *const *mut c_void,
) {
    let (_tx, img, buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    let width = img.get_width();
    let height = img.get_height();
    let format = img.get_format();
    img.set_data(width, height, false, format, buffer);
}

pub(super) unsafe extern "C" fn video_display_callback(
    opaque: *mut c_void,
    _picture: *mut c_void,
) {
    let (tx, img, _buffer) = (opaque
        as *mut (
            *mut mpsc::Sender<(bool, Gd<Image>)>,
            Gd<Image>,
            PackedByteArray,
        ))
        .as_mut()
        .unwrap();
    _ = tx
        .as_mut()
        .unwrap()
        .send((false, img.duplicate().unwrap().cast()));
}

pub(super) unsafe extern "C" fn video_format_callback(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    let tx: *mut mpsc::Sender<(bool, Gd<Image>)> = *opaque as *mut mpsc::Sender<(bool, Gd<Image>)>;
    let img = match Image::create_empty(*width as i32, *height as i32, false, image::Format::RGB8) {
        Some(img) => img,
        None => {
            return 0;
        }
    };
    let buffer = img.get_data();
    chroma.copy_from(c"RV24".as_ptr(), 5);
    *pitches = *width * 3;
    *lines = *height;
    if tx.as_mut().unwrap().send((true, img.clone())).is_err() {
        return 0;
    }
    *opaque = Box::into_raw(Box::new((tx, img, buffer))) as *mut c_void;
    1
}

pub(super) unsafe extern "C" fn video_cleanup_callback(opaque: *mut c_void) {
    let (_tx, _img, _buffer) = *Box::from_raw(
        opaque
            as *mut (
                *mut mpsc::Sender<(bool, Gd<Image>)>,
                Gd<Image>,
                PackedByteArray,
            ),
    );
}
