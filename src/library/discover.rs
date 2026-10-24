//! Finds PC games on the drives for the Add games screen. A game folder is one with an exe plus
//! the files games ship with: store runtimes (Steam, Epic, GOG, Ubisoft), Bink video, a Unity or
//! Unreal layout, upscaler or audio middleware, a redist folder. Installers, tools and the
//! launcher's own emulators don't have those, so they aren't offered.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

/// Deep enough for `E:\Yvan\Games\Some Game\bin\x64`, shallow enough to stay quick.
const MAX_DEPTH: usize = 6;
const MARKERS_NEEDED: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub title: String,
    pub folder: PathBuf,
    /// Best guess first; the screen lets the user pick another.
    pub exes: Vec<PathBuf>,
}

/// Paths already known to the launcher, compared case-insensitively like Windows does.
#[derive(Default)]
pub struct Known {
    /// Exes of games in the library: their folders aren't offered again.
    exes: Vec<String>,
    /// Folders not worth entering (emulators, the launcher's data).
    skip_dirs: HashSet<String>,
}

impl Known {
    pub fn new(exes: &[PathBuf], skip_dirs: &[PathBuf]) -> Known {
        Known {
            exes: exes.iter().map(|p| key(p)).collect(),
            skip_dirs: skip_dirs.iter().map(|p| key(p)).collect(),
        }
    }

    fn has_game_in(&self, folder: &Path) -> bool {
        let folder = format!("{}\\", key(folder));
        self.exes.iter().any(|exe| exe.starts_with(&folder))
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

/// Walks `roots` (drive roots or folders). `progress` gets the number of folders looked at so
/// far; `cancel` stops the walk early (the screen was closed).
pub fn scan(
    roots: &[PathBuf],
    known: &Known,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Vec<Candidate> {
    let mut walk = Walk {
        known,
        cancel,
        visited: 0,
        progress: &mut progress,
        found: Vec::new(),
    };
    for root in roots {
        walk.dir(root, 0);
    }
    walk.found
        .sort_by_cached_key(|c| (c.title.to_lowercase(), c.folder.clone()));
    walk.found
}

struct Walk<'a> {
    known: &'a Known,
    cancel: &'a AtomicBool,
    visited: usize,
    progress: &'a mut dyn FnMut(usize),
    found: Vec<Candidate>,
}

struct Listing {
    /// (lowercase name, path, size) of every file.
    files: Vec<(String, PathBuf, u64)>,
    /// (lowercase name, path) of every real folder (links and junctions are not followed).
    dirs: Vec<(String, PathBuf)>,
}

fn list(dir: &Path) -> Option<Listing> {
    let mut listing = Listing {
        files: Vec::new(),
        dirs: Vec::new(),
    };
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if kind.is_dir() {
            listing.dirs.push((name, entry.path()));
        } else if kind.is_file() {
            let size = entry.metadata().map_or(0, |m| m.len());
            listing.files.push((name, entry.path(), size));
        }
    }
    Some(listing)
}

impl Walk<'_> {
    fn dir(&mut self, dir: &Path, depth: usize) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        self.visited += 1;
        if self.visited.is_multiple_of(250) {
            (self.progress)(self.visited);
        }
        let Some(listing) = list(dir) else {
            return;
        };
        if let Some(candidate) = game_in(dir, &listing) {
            // A game folder holds the game; its subfolders are its data, not more games.
            if !self.known.has_game_in(dir) {
                self.found.push(candidate);
            }
            return;
        }
        if depth >= MAX_DEPTH {
            return;
        }
        for (name, path) in &listing.dirs {
            if !skip_dir(name) && !self.known.skip_dirs.contains(&key(path)) {
                self.dir(path, depth + 1);
            }
        }
    }
}

/// System, tool and cache folders: never games, and often huge.
fn skip_dir(name: &str) -> bool {
    const SKIP: &[&str] = &[
        "windows",
        "windowsapps",
        "xboxgames",
        "programdata",
        "system volume information",
        "recovery",
        "perflogs",
        "appdata",
        "node_modules",
        "msocache",
        "intel",
        "amd",
        "nvidia",
        "nvidia corporation",
        "drivers",
        "windows kits",
        "microsoft sdks",
        "microsoft visual studio",
        "microsoft",
        "common files",
        "windowspowershell",
        "reference assemblies",
        "dotnet",
        "packages",
        "temp",
        "tmp",
        "__pycache__",
        "site-packages",
        "inetpub",
        "xampp",
        "android",
        "jetbrains",
        "shadercache",
        "_commonredist",
        "_redist",
        "redist",
        "commonredist",
        "directx",
    ];
    name.starts_with('$') || name.starts_with('.') || SKIP.contains(&name)
}

fn game_in(dir: &Path, listing: &Listing) -> Option<Candidate> {
    let mut exes = exes_in(&listing.files);
    if exes.is_empty() {
        // Some games keep their exe one level down (bin, x64, Binaries\Win64).
        for (name, path) in &listing.dirs {
            if matches!(
                name.as_str(),
                "bin" | "bin64" | "x64" | "win64" | "binaries"
            ) {
                let Some(inner) = list(path) else { continue };
                exes = exes_in(&inner.files);
                if exes.is_empty()
                    && let Some((_, win64)) = inner.dirs.iter().find(|(n, _)| n == "win64")
                {
                    exes = list(win64).map(|l| exes_in(&l.files)).unwrap_or_default();
                }
                if !exes.is_empty() {
                    break;
                }
            }
        }
    }
    if exes.is_empty() || markers(listing, &exes) < MARKERS_NEEDED {
        return None;
    }
    let folder_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let exes = rank_exes(exes);
    let title = exe_title(&exes[0]).unwrap_or_else(|| clean_folder_title(&folder_name));
    Some(Candidate {
        title,
        folder: dir.to_path_buf(),
        exes,
    })
}

/// Exes worth launching, with their sizes. Installers, uninstallers, crash reporters,
/// anti-cheat services and redistributables are left out.
fn exes_in(files: &[(String, PathBuf, u64)]) -> Vec<(PathBuf, u64)> {
    const STARTS: &[&str] = &[
        "unins",
        "setup",
        "install",
        "uninstall",
        "vc_redist",
        "vcredist",
        "dxsetup",
        "dxwebsetup",
        "dotnet",
        "ndp",
        "oalinst",
        "physx",
        "ue4prereq",
        "ueprereq",
        "crs-",
        "unitycrashhandler",
        "crashpad",
        "easyanticheat",
        "battleye",
        "beservice",
        "eac",
        "quicksfv",
        "7z",
        "winrar",
        "unrar",
    ];
    const CONTAINS: &[&str] = &[
        "crash",
        "report",
        "helper",
        "updater",
        "update",
        "patch",
        "config",
        "settings",
        "uploader",
        "handler",
        "redist",
        "server",
        "editor",
        "benchmark",
        "cleanup",
        "touchup",
        "prereq",
        "subprocess",
        "cefprocess",
    ];
    files
        .iter()
        .filter_map(|(name, path, size)| {
            let stem = name.strip_suffix(".exe")?;
            let excluded = STARTS.iter().any(|s| stem.starts_with(s))
                || CONTAINS.iter().any(|c| stem.contains(c));
            (!excluded).then(|| (path.clone(), *size))
        })
        .collect()
}

/// Evidence that a folder is a game rather than an app.
fn markers(listing: &Listing, exes: &[(PathBuf, u64)]) -> u32 {
    let mut score = 0;
    for (name, _, size) in &listing.files {
        score += match name.as_str() {
            "steam_api.dll"
            | "steam_api64.dll"
            | "steam_emu.ini"
            | "steamclient64.dll"
            | "unityplayer.dll"
            | "bink2w64.dll"
            | "bink2w32.dll"
            | "binkw32.dll"
            | "binkw64.dll"
            | "eossdk-win64-shipping.dll"
            | "eossdk-win32-shipping.dll"
            | "galaxy.dll"
            | "galaxy64.dll"
            | "uplay_r1_loader.dll"
            | "uplay_r1_loader64.dll"
            | "upc_r2_loader64.dll"
            | "libscepad.dll" => 3,
            "nvngx_dlss.dll"
            | "amd_ags_x64.dll"
            | "ffx_fsr2_api_x64.dll"
            | "fmod.dll"
            | "fmod64.dll"
            | "fmodex.dll"
            | "fmodex64.dll"
            | "physx3_x64.dll"
            | "physxloader.dll"
            | "xinput1_3.dll" => 2,
            "d3dx9_43.dll" | "unins000.exe" => 1,
            n if n.starts_with("goggame-") => 3,
            n if n.starts_with("oo2core_") || n.starts_with("fmodstudio") => 2,
            // Big packed data: .pak (Unreal and others), .rpf (Rockstar), .forge (Ubisoft)…
            n if *size > 100 << 20
                && [
                    ".pak", ".rpf", ".forge", ".vpk", ".big", ".wad", ".arc", ".bin",
                ]
                .iter()
                .any(|ext| n.ends_with(ext)) =>
            {
                2
            }
            _ => 0,
        };
    }
    let stems: Vec<String> = exes
        .iter()
        .filter_map(|(p, _)| p.file_stem())
        .map(|s| s.to_string_lossy().to_lowercase())
        .collect();
    for (name, _) in &listing.dirs {
        score += match name.as_str() {
            // Unreal keeps an Engine folder beside the game's own folder.
            "engine" => 3,
            "_commonredist" | "_redist" | "commonredist" | "redist" | "directx" => 2,
            // Unity: Game.exe next to Game_Data.
            n if stems.iter().any(|s| n == format!("{s}_data")) => 3,
            _ => 0,
        };
    }
    score
}

/// Biggest first (the game, not its launcher stub); near-equal sizes go to the shorter name
/// (tlou-i.exe over tlou-i-l.exe).
fn rank_exes(mut exes: Vec<(PathBuf, u64)>) -> Vec<PathBuf> {
    exes.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    let biggest = exes.first().map_or(0, |(_, s)| *s);
    let stem_len = |p: &Path| p.file_stem().map_or(usize::MAX, |s| s.len());
    let (mut close, rest): (Vec<_>, Vec<_>) = exes
        .into_iter()
        .partition(|(_, size)| *size as f64 >= biggest as f64 * 0.9);
    close.sort_by_key(|(p, _)| stem_len(p));
    close
        .into_iter()
        .chain(rest)
        .map(|(p, _)| p)
        .take(5)
        .collect()
}

/// The exe's own name for the game: its description, else its product name, unless that is
/// generic ("Launcher", "Unreal Engine") or just the file name again ("GoW" for GoW.exe).
fn exe_title(exe: &Path) -> Option<String> {
    let stem = exe.file_stem()?.to_string_lossy().to_lowercase();
    ["FileDescription", "ProductName"]
        .into_iter()
        .filter_map(|field| version_string(exe, field))
        .map(|text| strip_marks(&text))
        .find(|text| !generic_title(text) && text.to_lowercase() != stem)
}

fn strip_marks(text: &str) -> String {
    text.replace(['™', '®', '©'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn generic_title(text: &str) -> bool {
    const GENERIC: &[&str] = &[
        "unreal",
        "unity",
        "bootstrap",
        "launcher",
        "microsoft",
        "windows",
        "engine",
        "client",
        "executable",
        "application",
        "shipping",
        "win64",
        "x64",
    ];
    let lower = text.to_lowercase();
    text.len() < 2
        || text.len() > 60
        || matches!(lower.as_str(), "game" | "app" | "main" | "player")
        || GENERIC.iter().any(|g| lower.contains(g))
}

/// A folder name read as a title: "God.of.War.RePack.by.Chovka" -> "God of War".
pub fn clean_folder_title(name: &str) -> String {
    const TAGS: &[&str] = &[
        "repack",
        "fitgirl",
        "dodi",
        "codex",
        "plaza",
        "skidrow",
        "empress",
        "cpy",
        "reloaded",
        "elamigos",
        "gog",
        "steamrip",
        "rune",
        "flt",
        "tenoke",
        "razor1911",
        "multi",
        "portable",
    ];
    let noise = |word: &str| {
        let lower = word.to_lowercase();
        let digits = lower.strip_prefix('v').unwrap_or(&lower);
        // v1.4, 1.4.0.0 (a bare "2" is part of a title: Portal 2).
        let version = !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
            && (lower.starts_with('v') || digits.contains('.'));
        version || TAGS.contains(&lower.as_str()) || lower == "build"
    };
    let mut text = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = (depth - 1).max(0),
            '_' if depth == 0 => text.push(' '),
            c if depth == 0 => text.push(c),
            _ => {}
        }
    }
    // Versions and groups first, while dots still hold "1.4.0.0" together; a dash joins
    // them to the name ("v1.4-GOG") but also belongs to names ("Spider-Man"), so only noisy
    // parts are dropped.
    let text: Vec<String> = text
        .split_whitespace()
        .map(|token| {
            let parts: Vec<&str> = token.split('-').filter(|p| !noise(p)).collect();
            parts.join("-").replace('.', " ")
        })
        .collect();
    let joined = text.join(" ");
    let words: Vec<&str> = joined.split_whitespace().collect();
    let mut kept = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if words[i].eq_ignore_ascii_case("by") {
            // "RePack by Chovka": the group's name goes with it.
            i += 2;
            continue;
        }
        if !noise(words[i]) {
            kept.push(words[i]);
        }
        i += 1;
    }
    let title = kept.join(" ");
    let title = title.trim_matches(|c: char| c == '-' || c.is_whitespace());
    if title.is_empty() {
        name.to_string()
    } else {
        title.to_string()
    }
}

/// A string from the exe's version resource (what Explorer shows under Details).
fn version_string(exe: &Path, field: &str) -> Option<String> {
    let path = wide(&exe.to_string_lossy());
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        // The first language listed, then US English as found in most games.
        let mut languages = Vec::new();
        let mut pointer: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        if VerQueryValueW(
            data.as_ptr().cast(),
            wide("\\VarFileInfo\\Translation").as_ptr(),
            &mut pointer,
            &mut len,
        ) != 0
            && len >= 4
        {
            let pair = std::slice::from_raw_parts(pointer as *const u16, 2);
            languages.push(format!("{:04x}{:04x}", pair[0], pair[1]));
        }
        languages.extend(["040904b0".to_string(), "040904e4".to_string()]);
        for language in languages {
            let query = wide(&format!("\\StringFileInfo\\{language}\\{field}"));
            let mut text: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut chars: u32 = 0;
            if VerQueryValueW(data.as_ptr().cast(), query.as_ptr(), &mut text, &mut chars) != 0
                && chars > 1
            {
                let slice = std::slice::from_raw_parts(text as *const u16, chars as usize);
                let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
                let value = String::from_utf16_lossy(&slice[..end]).trim().to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
        None
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("gl-discover-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Tree(root)
        }

        /// Creates a file of `size` bytes (sizes decide which exe is the game).
        fn file(&self, relative: &str, size: u64) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::File::create(&path).unwrap().set_len(size).unwrap();
            path
        }

        fn scan(&self, known: &Known) -> Vec<Candidate> {
            scan(
                std::slice::from_ref(&self.0),
                known,
                &AtomicBool::new(false),
                |_| {},
            )
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_a_repack_install_and_picks_the_game_exe() {
        // Shaped like Yvan's E:\Games\God of War and C:\Yvan\Games\The Last of Us - Part I.
        let tree = Tree::new("games");
        tree.file(r"Games\God of War\GoW.exe", 19 << 20);
        tree.file(r"Games\God of War\unins000.exe", 1 << 20);
        tree.file(r"Games\God of War\steam_api64.dll", 300_000);
        tree.file(r"Games\God of War\bink2w64.dll", 400_000);
        tree.file(r"Games\The Last of Us - Part I\tlou-i.exe", 58 << 20);
        tree.file(r"Games\The Last of Us - Part I\tlou-i-l.exe", 58 << 20);
        tree.file(r"Games\The Last of Us - Part I\launcher.exe", 1 << 20);
        tree.file(r"Games\The Last of Us - Part I\crs-handler.exe", 1 << 20);
        tree.file(r"Games\The Last of Us - Part I\nvngx_dlss.dll", 1000);
        tree.file(r"Games\The Last of Us - Part I\_Redist\x.txt", 10);
        let found = tree.scan(&Known::default());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].title, "God of War");
        assert!(found[0].exes[0].ends_with("GoW.exe"));
        assert!(!found[0].exes.iter().any(|e| e.ends_with("unins000.exe")));
        assert_eq!(found[1].title, "The Last of Us - Part I");
        assert!(
            found[1].exes[0].ends_with("tlou-i.exe"),
            "{:?}",
            found[1].exes
        );
        assert!(found[1].exes.iter().any(|e| e.ends_with("launcher.exe")));
    }

    #[test]
    fn installers_and_apps_are_not_games() {
        let tree = Tree::new("noise");
        // A repack waiting to be installed: Setup.exe and archives only.
        tree.file(r"Games\God.of.War.RePack.by.Chovka\Setup.exe", 2 << 20);
        tree.file(r"Games\God.of.War.RePack.by.Chovka\Data1.RePack", 1000);
        // An ordinary app: an exe and some DLLs, no game markers.
        tree.file(r"Tools\Editor\notes.exe", 5 << 20);
        tree.file(r"Tools\Editor\Qt6Core.dll", 1 << 20);
        assert!(tree.scan(&Known::default()).is_empty());
    }

    #[test]
    fn unity_and_unreal_layouts_are_recognised() {
        let tree = Tree::new("engines");
        tree.file(r"Hollow Knight\hollow_knight.exe", 600_000);
        tree.file(r"Hollow Knight\hollow_knight_Data\level0", 10);
        tree.file(r"Hollow Knight\UnityPlayer.dll", 20 << 20);
        tree.file(r"Stray\Stray.exe", 300_000);
        tree.file(r"Stray\Engine\Binaries\x.dll", 10);
        tree.file(
            r"Stray\Hk_project\Binaries\Win64\Stray-Win64-Shipping.exe",
            90 << 20,
        );
        let found = tree.scan(&Known::default());
        let titles: Vec<&str> = found.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Hollow Knight", "Stray"]);
    }

    #[test]
    fn games_already_in_the_library_and_skipped_folders_are_left_out() {
        let tree = Tree::new("known");
        let exe = tree.file(r"Games\Hades\Hades.exe", 10 << 20);
        tree.file(r"Games\Hades\steam_api64.dll", 10);
        tree.file(r"Emu\Games\Celeste\Celeste.exe", 10 << 20);
        tree.file(r"Emu\Games\Celeste\steam_api.dll", 10);
        let known = Known::new(&[exe], &[tree.0.join("EMU")]);
        assert!(tree.scan(&known).is_empty(), "case-insensitive paths");
    }

    #[test]
    fn folder_names_become_titles() {
        assert_eq!(
            clean_folder_title("God.of.War.RePack.by.Chovka"),
            "God of War"
        );
        assert_eq!(clean_folder_title("Hades [FitGirl Repack]"), "Hades");
        assert_eq!(clean_folder_title("Celeste_v1.4.0.0-GOG"), "Celeste");
        assert_eq!(
            clean_folder_title("The Last of Us - Part I"),
            "The Last of Us - Part I"
        );
        assert_eq!(clean_folder_title("(USA)"), "(USA)", "never empty");
    }

    #[test]
    fn generic_names_are_not_titles() {
        assert!(generic_title("Unreal Engine"));
        assert!(generic_title("Launcher"));
        assert!(!generic_title("The Last of Us Part I"));
        assert_eq!(
            strip_marks("The Last of Us™  Part I"),
            "The Last of Us Part I"
        );
    }

    /// Reads (never writes) every drive of this PC and prints what it would offer:
    /// `cargo test real_drives -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_drives() {
        let roots: Vec<PathBuf> = crate::library::drives::scannable()
            .into_iter()
            .map(|l| PathBuf::from(format!("{l}:\\")))
            .collect();
        let started = std::time::Instant::now();
        let mut folders = 0;
        let found = scan(&roots, &Known::default(), &AtomicBool::new(false), |n| {
            folders = n
        });
        println!("{roots:?}: ~{folders} folders in {:?}", started.elapsed());
        for c in &found {
            println!("  {}  <-  {}", c.title, c.exes[0].display());
        }
    }

    #[test]
    fn a_cancelled_scan_stops() {
        let tree = Tree::new("cancel");
        tree.file(r"Games\Hades\Hades.exe", 10 << 20);
        tree.file(r"Games\Hades\steam_api64.dll", 10);
        let cancelled = AtomicBool::new(true);
        assert!(
            scan(
                std::slice::from_ref(&tree.0),
                &Known::default(),
                &cancelled,
                |_| {}
            )
            .is_empty()
        );
    }
}
