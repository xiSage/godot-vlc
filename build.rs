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

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap();

    if target.contains("windows") && target.contains("x86_64") {
        println!("cargo:rustc-link-search=./thirdparty/vlc/lib/win64");
    }

    println!("cargo:rustc-link-lib=libvlc");
    println!("cargo:rustc-link-lib=libvlccore");

    let bindings = bindgen::Builder::default()
        .header("thirdparty/vlc/include/vlc/vlc.h")
        .clang_arg("-Ithirdparty/vlc/include")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate_cstr(true)
        .disable_header_comment()
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("vlc_bindings.rs"))
        .expect("Couldn't write bindings!");
}
