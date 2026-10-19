use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=t5-sdk/include/TiltFiveNative.h");
    println!("cargo:rerun-if-changed=t5-sdk/include/types.h");
    println!("cargo:rerun-if-changed=t5-sdk/include/errors.h");

    let bindings = bindgen::Builder::default()
        .header("t5-sdk/include/TiltFiveNative.h")
        .dynamic_library_name("TiltFiveNative")
        .dynamic_link_require_all(false)
        .allowlist_function("t5.*")
        .allowlist_type("T5_.*")
        .allowlist_var("T5_.*|kT5_.*")
        .derive_default(true)
        .derive_debug(true)
        .derive_copy(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings for TiltFiveNative.h");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
