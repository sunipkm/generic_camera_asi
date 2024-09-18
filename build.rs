extern crate bindgen;
extern crate cc;

use std::{env, path::PathBuf};

use bindgen::CargoCallbacks;

#[cfg(target_os = "windows")]
compile_error!("generic-camera-asi does not support Windows");

fn main() {
    // This is the directory where the `c` library is located.
    // Canonicalize the path as `rustc-link-search` requires an absolute path.
    let libdir_path = PathBuf::from("include")
        .canonicalize()
        .expect("cannot canonicalize path");

    // This is the path to the `c` headers file.
    let headers_path = libdir_path.join("wrapper.h");

    let headers_path_str = headers_path.to_str().expect("Path is not a valid string");

    // Tell cargo to tell rustc to find ASICamera2 in LD_LIBRARY_PATH on Linux. This is not
    // an issue on macOS.
    #[cfg(target_os = "linux")]
    {
        if let Ok(libdir) = std::env::var("LD_LIBRARY_PATH") {
            let paths = libdir
                .split(":")
                .filter(|x| !x.is_empty())
                .collect::<Vec<&str>>();
            for path in paths {
                println!("cargo:rustc-link-search={}", path);
            }
        } else {
            panic!(
                "LD_LIBRARY_PATH is not set. Please set it to the directory containing ASICamera2: LD_LIBRARY_PATH=/path/to/ASICamera2:$LD_LIBRARY_PATH"
            );
        }
    }
    println!("cargo:rustc-link-lib=static=ASICamera2");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=usb-1.0");
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=stdc++");
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=c++");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .header(headers_path_str)
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(CargoCallbacks))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    let out_path = out_path.join("bindings.rs");

    bindings
        .write_to_file(out_path)
        .expect("Couldn't write bindings!");

    // cc::Build::new()
    //     .file("src/lib.c")
    //     .compile("cameraunit_asi");
}
