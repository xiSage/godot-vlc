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
use std::path::{Path, PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap();
    let mut include_dir = "";
    let mut clang_args: Vec<String> = Vec::new();

    // Android is tested first and on its own. Its triple is
    // aarch64-linux-android, which also contains "linux", so the Linux branch
    // below would otherwise match and link an Android build against the desktop
    // runtime -- a mismatch that builds cleanly and fails at load time.
    if target.contains("android") {
        if !target.contains("aarch64") {
            panic!(
                "only the arm64 Android target has a staged runtime: expected an \
                 'aarch64' triple, got '{target}'. Stage thirdparty/vlc/android-<arch>/ \
                 and add the branch here before building for it."
            );
        }
        println!("cargo:rustc-link-search=./thirdparty/vlc/android-arm64/lib");
        include_dir = "thirdparty/vlc/android-arm64/include";

        // bindgen runs clang, and clang has to be told what it is compiling for.
        // Without a sysroot it reaches for the host's C library headers and stops
        // at the first `#include <stdio.h>` in vlc/libvlc.h, which reads as if the
        // VLC headers were broken. The NDK carries that sysroot, in a prebuilt
        // directory named after the host, so the NDK has to be pointed at.
        //
        // This mirrors what the rest of the build already needs: LIBCLANG_PATH
        // for bindgen and the NDK's compilers for the code the extension links.
        let ndk = env::var("ANDROID_NDK_HOME")
            .or_else(|_| env::var("ANDROID_NDK_ROOT"))
            .unwrap_or_else(|_| {
                panic!(
                    "set ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) to the Android NDK: \
                     the bindings are generated against its sysroot"
                )
            });
        let host = env::var("HOST").unwrap();
        let host_tag = if host.contains("windows") {
            "windows-x86_64"
        } else if host.contains("darwin") {
            "darwin-x86_64"
        } else {
            "linux-x86_64"
        };
        let sysroot = Path::new(&ndk)
            .join("toolchains/llvm/prebuilt")
            .join(host_tag)
            .join("sysroot");
        assert!(
            sysroot.is_dir(),
            "{} is not a directory, so ANDROID_NDK_HOME does not name an Android NDK",
            sysroot.display()
        );

        clang_args.push(format!("--target={target}"));
        clang_args.push(format!("--sysroot={}", sysroot.display()));
    } else if target.contains("windows") && target.contains("x86_64") {
        println!("cargo:rustc-link-search=./thirdparty/vlc/win-x64/lib");
        include_dir = "thirdparty/vlc/win-x64/include";
    } else if target.contains("linux") && target.contains("x86_64") {
        println!("cargo:rustc-link-search=./thirdparty/vlc/linux-x64/lib");
        include_dir = "thirdparty/vlc/linux-x64/include";
    }

    println!("cargo:rustc-link-lib=vlc");

    let bindings = bindgen::Builder::default()
        .header(format!("{}/vlc/vlc.h", include_dir))
        .clang_arg(format!("-I{}", include_dir))
        .clang_args(clang_args)
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
