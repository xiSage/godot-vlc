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
    classes::{Image, image},
    prelude::*,
};

use super::output_sink::OutputSink;

/// One video output's picture, and where its frames go.
///
/// The sender is owned rather than borrowed from the player: this state lives as
/// long as the video output does, and that is not the same as the player's
/// lifetime -- the player can be freed first (a media list player retains it), and
/// then a borrowed address would be one into a freed object. An output that is
/// opened after that gets no sender at all and simply delivers nowhere.
struct SoftwareVideoState {
    tx: Option<mpsc::Sender<(bool, Gd<Image>)>>,
    img: Gd<Image>,
    buffer: PackedByteArray,
}

pub(super) unsafe extern "C" fn video_lock_callback(
    opaque: *mut c_void,
    planes: *mut *mut c_void,
) -> *mut c_void {
    unsafe {
        let state = (opaque as *mut SoftwareVideoState).as_mut().unwrap();
        let buffer_ptr = state.buffer.as_mut_slice().as_mut_ptr();
        *planes = buffer_ptr as *mut c_void;
        ptr::null_mut()
    }
}

pub(super) unsafe extern "C" fn video_unlock_callback(
    opaque: *mut c_void,
    _picture: *mut c_void,
    _planes: *const *mut c_void,
) {
    unsafe {
        let state = (opaque as *mut SoftwareVideoState).as_mut().unwrap();
        let width = state.img.get_width();
        let height = state.img.get_height();
        let format = state.img.get_format();
        state
            .img
            .set_data(width, height, false, format, &state.buffer);
    }
}

pub(super) unsafe extern "C" fn video_display_callback(opaque: *mut c_void, _picture: *mut c_void) {
    unsafe {
        let state = (opaque as *mut SoftwareVideoState).as_mut().unwrap();
        // No sender is the player having been freed while this output was still
        // running; the frame is drawn for nobody rather than sent to a dead channel.
        if let Some(tx) = &state.tx {
            _ = tx.send((false, Gd::duplicate_resource(&state.img).cast()));
        }
    }
}

pub(super) unsafe extern "C" fn video_format_callback(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    unsafe {
        // `*opaque` is the player-level address until this callback stores the
        // state over it: libvlc keeps one opaque per output and hands its address
        // here, and it is the sink that was passed to `libvlc_video_set_callbacks`.
        let sink = OutputSink::from_opaque(*opaque);
        let tx = sink.video_sender();
        let img =
            match Image::create_empty(*width as i32, *height as i32, false, image::Format::RGB8) {
                Some(img) => img,
                None => {
                    return 0;
                }
            };
        let buffer = img.get_data();
        chroma.copy_from(c"RV24".as_ptr(), 5);
        *pitches = *width * 3;
        *lines = *height;
        // A closed sink is not a reason to refuse the output: the picture is
        // accepted and drawn into nothing, which is what keeps a list player
        // playing the next item instead of tearing this output down.
        if let Some(tx) = &tx
            && tx.send((true, img.clone())).is_err()
        {
            return 0;
        }
        *opaque = Box::into_raw(Box::new(SoftwareVideoState { tx, img, buffer })) as *mut c_void;
        1
    }
}

pub(super) unsafe extern "C" fn video_cleanup_callback(opaque: *mut c_void) {
    unsafe {
        let _state = *Box::from_raw(opaque as *mut SoftwareVideoState);
    }
}
