use godot::prelude::*;
use std::{ffi::CString, num::NonZeroU8};

pub fn cstring_to_gstring(str: GString) -> CString {
    CString::from(
        str.to_utf8_buffer()
            .as_slice()
            .iter()
            .map_while(|x| NonZeroU8::new(*x))
            .collect::<Vec<NonZeroU8>>(),
    )
}
