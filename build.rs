use std::{env, fs, path::PathBuf};

fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI failed to compile");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let sdl_dir = manifest_dir
        .join("vendor")
        .join("SDL3")
        .join("lib")
        .join("x64");
    println!("cargo:rustc-link-search=native={}", sdl_dir.display());
    println!("cargo:rerun-if-changed=vendor/SDL3/lib/x64/SDL3.dll");

    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out. The exe lives in target/<profile>
    // and test binaries in target/<profile>/deps; both need SDL3.dll beside them to start.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR layout");
    for dest in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
        fs::create_dir_all(&dest).unwrap();
        fs::copy(sdl_dir.join("SDL3.dll"), dest.join("SDL3.dll")).expect("failed to copy SDL3.dll");
    }
}
