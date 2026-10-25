//! Reads one file out of an ISO 9660 disc image (PS3 ISOs use it), without mounting it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECTOR: u64 = 2048;
/// Directories this big are not a real disc: a corrupt image must not allocate gigabytes.
const MAX_DIRECTORY: u32 = 16 * 1024 * 1024;
/// Neither are files this big for the small files the launcher reads (menu music, icons).
const MAX_FILE: u32 = 64 * 1024 * 1024;

/// The file at `path` (folder names then the file name, any case) inside the image, or None
/// when the image doesn't have it.
pub fn read_file(image: &Path, path: &[&str]) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = File::open(image)?;
    read_file_from(&mut file, path)
}

fn read_file_from(
    image: &mut (impl Read + Seek),
    path: &[&str],
) -> std::io::Result<Option<Vec<u8>>> {
    let invalid =
        |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    // The primary volume descriptor sits in sector 16; its root directory record at byte 156.
    let descriptor = read_at(image, 16 * SECTOR, SECTOR as usize)?;
    if &descriptor[1..6] != b"CD001" || descriptor[0] != 1 {
        return Err(invalid("not an ISO 9660 image"));
    }
    let mut entry = Record::parse(&descriptor[156..190]).ok_or_else(|| invalid("bad root"))?;
    for (depth, name) in path.iter().enumerate() {
        if !entry.directory || entry.size > MAX_DIRECTORY {
            return Ok(None);
        }
        let listing = read_at(image, entry.sector as u64 * SECTOR, entry.size as usize)?;
        let Some(found) = find(&listing, name) else {
            return Ok(None);
        };
        let last = depth + 1 == path.len();
        if last == found.directory {
            return Ok(None);
        }
        entry = found;
    }
    if entry.size > MAX_FILE {
        return Err(invalid("file too large"));
    }
    read_at(image, entry.sector as u64 * SECTOR, entry.size as usize).map(Some)
}

struct Record {
    sector: u32,
    size: u32,
    directory: bool,
    name: Vec<u8>,
}

impl Record {
    /// A directory record: length, extent (little-endian at 2), size (at 10), flags (at 25),
    /// name length (at 32) and name (from 33).
    fn parse(bytes: &[u8]) -> Option<Record> {
        let length = *bytes.first()? as usize;
        if length < 34 || bytes.len() < length {
            return None;
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let name_len = bytes[32] as usize;
        let name = bytes.get(33..33 + name_len)?.to_vec();
        Some(Record {
            sector: u32_at(2),
            size: u32_at(10),
            directory: bytes[25] & 2 != 0,
            name,
        })
    }
}

/// The entry named `name` in a directory listing. Records never cross a sector boundary; a zero
/// length means the rest of that sector is padding. Names compare without case or ";1".
fn find(listing: &[u8], name: &str) -> Option<Record> {
    let mut at = 0;
    while at < listing.len() {
        let length = listing[at] as usize;
        if length == 0 {
            at = (at / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        let record = Record::parse(&listing[at..])?;
        let entry_name = String::from_utf8_lossy(&record.name);
        let entry_name = entry_name.split(';').next().unwrap_or_default();
        if entry_name.eq_ignore_ascii_case(name) {
            return Some(record);
        }
        at += length;
    }
    None
}

fn read_at(image: &mut (impl Read + Seek), offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    image.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0; len];
    image.read_exact(&mut buffer)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn record(name: &[u8], sector: u32, size: u32, directory: bool) -> Vec<u8> {
        let length = 33 + name.len() + (name.len() + 1) % 2;
        let mut r = vec![0u8; length];
        r[0] = length as u8;
        r[2..6].copy_from_slice(&sector.to_le_bytes());
        r[6..10].copy_from_slice(&sector.to_be_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = if directory { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// A tiny disc: root (sector 18) holds PS3_GAME (sector 19), which holds SND0.AT3;1 (20).
    fn tiny_disc() -> Vec<u8> {
        let mut disc = vec![0u8; 21 * SECTOR as usize];
        let sector = |n: usize| n * SECTOR as usize;
        disc[sector(16)] = 1;
        disc[sector(16) + 1..sector(16) + 6].copy_from_slice(b"CD001");
        let root = record(&[0], 18, SECTOR as u32, true);
        disc[sector(16) + 156..sector(16) + 156 + root.len()].copy_from_slice(&root);
        let mut listing = record(&[0], 18, SECTOR as u32, true);
        listing.extend(record(&[1], 18, SECTOR as u32, true));
        listing.extend(record(b"PS3_GAME", 19, SECTOR as u32, true));
        disc[sector(18)..sector(18) + listing.len()].copy_from_slice(&listing);
        let mut game = record(&[0], 19, SECTOR as u32, true);
        game.extend(record(b"PARAM.SFO;1", 20, 4, false));
        game.extend(record(b"SND0.AT3;1", 20, 11, false));
        disc[sector(19)..sector(19) + game.len()].copy_from_slice(&game);
        disc[sector(20)..sector(20) + 11].copy_from_slice(b"RIFF music!");
        disc
    }

    #[test]
    fn reads_a_file_from_a_folder_inside_the_image() {
        let mut disc = Cursor::new(tiny_disc());
        let music = read_file_from(&mut disc, &["PS3_GAME", "SND0.AT3"]).unwrap();
        assert_eq!(music.as_deref(), Some(&b"RIFF music!"[..]));
        let any_case = read_file_from(&mut disc, &["ps3_game", "snd0.at3"]).unwrap();
        assert!(any_case.is_some());
    }

    #[test]
    fn missing_files_and_wrong_kinds_are_none() {
        let mut disc = Cursor::new(tiny_disc());
        assert_eq!(
            read_file_from(&mut disc, &["PS3_GAME", "PIC1.PNG"]).unwrap(),
            None
        );
        assert_eq!(
            read_file_from(&mut disc, &["NOPE", "SND0.AT3"]).unwrap(),
            None
        );
        // A folder asked for as a file, and a file asked for as a folder.
        assert_eq!(read_file_from(&mut disc, &["PS3_GAME"]).unwrap(), None);
        assert_eq!(
            read_file_from(&mut disc, &["PS3_GAME", "SND0.AT3", "X"]).unwrap(),
            None
        );
    }

    #[test]
    fn other_files_are_not_disc_images() {
        let mut junk = Cursor::new(vec![7u8; 40 * SECTOR as usize]);
        assert!(read_file_from(&mut junk, &["PS3_GAME", "SND0.AT3"]).is_err());
    }
}
