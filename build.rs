use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-link-search=./thirdparty/vlc/lib");
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