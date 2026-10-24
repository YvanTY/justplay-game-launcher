use std::{env, fs, path::PathBuf};

fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI failed to compile");
    embed_icon();

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

/// Compiles assets/icon.ico into a Windows resource and links it, so Explorer, the taskbar and
/// shortcuts show the launcher's icon. A missing resource compiler only costs the icon, not the build.
fn embed_icon() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("assets").join("icon.ico");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe (Windows SDK) not found; the exe will have no icon");
        return;
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let script = out_dir.join("icon.rc");
    let resource = out_dir.join("icon.res");
    // Resource id 1: Windows uses the first icon group as the exe's icon.
    // rc.exe treats backslashes in strings as escapes, so give it forward slashes.
    let icon_path = icon.display().to_string().replace('\\', "/");
    fs::write(&script, format!("1 ICON \"{icon_path}\"\n")).unwrap();
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&script)
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bins={}", resource.display())
        }
        other => println!("cargo:warning=rc.exe failed ({other:?}); the exe will have no icon"),
    }
}

/// The newest x64 rc.exe from the installed Windows 10/11 SDKs.
fn find_rc() -> Option<PathBuf> {
    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut candidates: Vec<PathBuf> = fs::read_dir(&kits)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("x64").join("rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}
