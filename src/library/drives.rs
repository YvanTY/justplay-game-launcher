//! Drive letters: which drive a path lives on, whether it is connected, and its name.

use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};

// GetDriveTypeW results (WinBase.h); defined here rather than enabling another windows-sys module.
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

/// Bit 0 is A:, bit 1 is B:, and so on. One cheap call, so it can be polled to notice plugging.
pub fn mask() -> u32 {
    unsafe { GetLogicalDrives() }
}

/// `E` for `E:\Games\x.exe`; None for relative and network paths.
pub fn letter_of(path: &Path) -> Option<char> {
    let text = path.to_str()?;
    let mut chars = text.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    (letter.is_ascii_alphabetic() && chars.next() == Some(':')).then_some(letter)
}

/// A card reader with no card has a letter but no root folder, so the folder is what counts.
pub fn is_connected(letter: char) -> bool {
    Path::new(&format!("{letter}:\\")).is_dir()
}

/// Internal disks, USB disks and sticks: the drives worth scanning for games. Network shares and
/// optical drives are left out (slow, and games aren't installed there).
pub fn scannable() -> Vec<char> {
    letters(mask())
        .filter(|&l| {
            let kind = unsafe { GetDriveTypeW(wide(&format!("{l}:\\")).as_ptr()) };
            (kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE) && is_connected(l)
        })
        .collect()
}

/// The name shown in Explorer, e.g. "Nouveau nom"; None when it has none.
pub fn label(letter: char) -> Option<String> {
    let root = wide(&format!("{letter}:\\"));
    let mut name = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let label = String::from_utf16_lossy(&name[..len]).trim().to_string();
    (ok != 0 && !label.is_empty()).then_some(label)
}

fn letters(mask: u32) -> impl Iterator<Item = char> {
    (0..26u8)
        .filter(move |i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_come_from_paths() {
        assert_eq!(letter_of(Path::new(r"e:\Games\GoW.exe")), Some('E'));
        assert_eq!(letter_of(Path::new(r"\\nas\games\x.exe")), None);
        assert_eq!(letter_of(Path::new("relative.exe")), None);
    }

    #[test]
    fn the_system_drive_is_connected_and_scannable() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = system.chars().next().unwrap();
        assert!(is_connected(letter));
        assert!(scannable().contains(&letter));
    }
}
