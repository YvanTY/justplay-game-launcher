//! The Choose art screen: for one game, every cover, background and logo found (the art in use,
//! Steam's images for several store matches, screenshots, the store banner, PCGamingWiki's
//! cover) in three rows. UI-free so it can be unit-tested; main.rs downloads the choices,
//! applies the one picked and mirrors this into the window.

use std::path::PathBuf;

use crate::Action;

/// Art folder file names, one per row.
pub const SLOTS: [&str; 3] = ["icon", "hero", "logo"];

/// Choices skipped by LT / RT.
const PAGE: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// The image file; None is the "None" choice (title text, title tile or plain color).
    pub path: Option<PathBuf>,
    /// Where it comes from, e.g. "Steam · Grand Theft Auto V Enhanced".
    pub label: String,
    pub in_use: bool,
    /// The sharp version, fetched only if this choice is picked (the file is a light preview).
    pub full: Option<String>,
    /// Visual fingerprint (`picture_hash`), so the same picture from two sources, or at two
    /// sizes, is offered once.
    hash: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Use this choice for this row's slot.
    Apply {
        slot: usize,
        index: usize,
    },
    Close,
}

pub struct ArtPicker {
    pub game: String,
    /// Tells this opening of the screen from an earlier one still finishing downloads.
    pub session: String,
    pub title: String,
    /// The game's art folder, where the picked file goes.
    pub art_dir: PathBuf,
    /// 0 cover, 1 background, 2 logo.
    pub slot: usize,
    pub choices: [Vec<Choice>; 3],
    pub focus: [usize; 3],
    /// Choices are still being looked for and downloaded.
    pub searching: bool,
}

impl ArtPicker {
    pub fn new(game: String, title: String, art_dir: PathBuf) -> ArtPicker {
        let none = |label: &str| {
            vec![Choice {
                path: None,
                label: label.into(),
                in_use: false,
                full: None,
                hash: 0,
            }]
        };
        ArtPicker {
            session: game.clone(),
            game,
            title,
            art_dir,
            slot: 0,
            choices: [
                none("None · show the title on a colored tile"),
                none("None · plain background"),
                none("None · show the title as text"),
            ],
            focus: [0; 3],
            searching: true,
        }
    }

    /// Adds a choice before "None" and returns where it went, or None when the same picture is
    /// already offered. The art in use goes first and takes the focus.
    pub fn add(
        &mut self,
        slot: usize,
        path: PathBuf,
        label: String,
        hash: u64,
        in_use: bool,
        full: Option<String>,
    ) -> Option<usize> {
        let row = &mut self.choices[slot];
        if row
            .iter()
            .any(|c| c.path.is_some() && same_picture(c.hash, hash))
        {
            return None;
        }
        let index = if in_use { 0 } else { row.len() - 1 };
        if in_use {
            for choice in row.iter_mut() {
                choice.in_use = false;
            }
        }
        row.insert(
            index,
            Choice {
                path: Some(path),
                label,
                in_use,
                full,
                hash,
            },
        );
        if in_use {
            self.focus[slot] = 0;
        } else if self.focus[slot] >= index {
            // Keep the focus on the same choice as the row grows under it.
            self.focus[slot] += 1;
        }
        Some(index)
    }

    /// The "None" choice is in use (e.g. the game shows its title instead of a logo).
    pub fn none_in_use(&mut self, slot: usize) {
        if let Some(none) = self.choices[slot].last_mut() {
            none.in_use = true;
        }
        self.focus[slot] = self.choices[slot].len() - 1;
    }

    pub fn applied(&mut self, slot: usize, index: usize) {
        for (i, choice) in self.choices[slot].iter_mut().enumerate() {
            choice.in_use = i == index;
        }
    }

    pub fn handle(&mut self, action: Action) -> Option<Effect> {
        let last = self.choices[self.slot].len() - 1;
        let focus = &mut self.focus[self.slot];
        match action {
            Action::Up | Action::TabPrev => self.slot = self.slot.saturating_sub(1),
            Action::Down | Action::TabNext => self.slot = (self.slot + 1).min(2),
            Action::Left => *focus = focus.saturating_sub(1),
            Action::Right => *focus = (*focus + 1).min(last),
            Action::PagePrev => *focus = focus.saturating_sub(PAGE),
            Action::PageNext => *focus = (*focus + PAGE).min(last),
            Action::Confirm => {
                let index = *focus;
                return (!self.choices[self.slot][index].in_use).then_some(Effect::Apply {
                    slot: self.slot,
                    index,
                });
            }
            Action::Back => return Some(Effect::Close),
            _ => {}
        }
        None
    }
}

/// A 64-bit fingerprint of what the picture looks like (each bit: is this cell of an 8x8
/// grayscale shrink brighter than average?). The same art at 600x900 and 300x450 gets the same
/// fingerprint; files that can't be decoded fall back to a hash of their bytes.
pub fn picture_hash(bytes: &[u8]) -> u64 {
    let Ok(image) = image::load_from_memory(bytes) else {
        return bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        });
    };
    let small = image
        .resize_exact(8, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mean = small.pixels().map(|p| p.0[0] as u32).sum::<u32>() / 64;
    small.pixels().enumerate().fold(0, |bits, (i, p)| {
        bits | (((p.0[0] as u32 > mean) as u64) << i)
    })
}

/// Fingerprints a few bits apart are the same picture (resizing shifts some cells slightly).
fn same_picture(a: u64, b: u64) -> bool {
    (a ^ b).count_ones() <= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fingerprints far apart, so only equal numbers count as the same picture.
    fn h(n: u64) -> u64 {
        n * 0x1111_1111_1111_1111
    }

    fn picker() -> ArtPicker {
        ArtPicker::new("pc:GTA".into(), "GTA".into(), PathBuf::from(r"C:\art\GTA"))
    }

    #[test]
    fn the_art_in_use_comes_first_and_none_stays_last() {
        let mut p = picker();
        assert_eq!(
            p.add(0, "a.jpg".into(), "Steam · A".into(), h(1), false, None),
            Some(0)
        );
        assert_eq!(
            p.add(0, "now.jpg".into(), "In use".into(), h(2), true, None),
            Some(0)
        );
        assert_eq!(
            p.add(0, "b.jpg".into(), "Steam · B".into(), h(3), false, None),
            Some(2)
        );
        let labels: Vec<&str> = p.choices[0].iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels[..3], ["In use", "Steam · A", "Steam · B"]);
        assert!(p.choices[0].last().unwrap().path.is_none());
        assert_eq!(p.focus[0], 0, "starts on the art in use");
    }

    #[test]
    fn the_same_picture_is_offered_once() {
        let mut p = picker();
        p.add(
            1,
            "legacy.jpg".into(),
            "Steam · Legacy".into(),
            h(7),
            false,
            None,
        );
        assert_eq!(
            p.add(
                1,
                "enhanced.jpg".into(),
                "Steam · Enhanced".into(),
                h(7),
                false,
                None
            ),
            None
        );
        assert_eq!(p.choices[1].len(), 2);
    }

    #[test]
    fn focus_follows_its_choice_as_the_row_grows() {
        let mut p = picker();
        p.add(2, "a.png".into(), "A".into(), h(1), false, None);
        p.handle(Action::Down);
        p.handle(Action::Down); // logo row
        p.handle(Action::Right); // on "None"
        assert!(p.choices[2][p.focus[2]].path.is_none());
        p.add(2, "b.png".into(), "B".into(), h(2), false, None);
        assert!(p.choices[2][p.focus[2]].path.is_none(), "still on None");
    }

    #[test]
    fn a_uses_a_choice_b_closes() {
        let mut p = picker();
        p.add(0, "now.jpg".into(), "In use".into(), h(1), true, None);
        p.add(0, "new.jpg".into(), "Steam".into(), h(2), false, None);
        assert_eq!(p.handle(Action::Confirm), None, "already in use");
        p.handle(Action::Right);
        assert_eq!(
            p.handle(Action::Confirm),
            Some(Effect::Apply { slot: 0, index: 1 })
        );
        p.applied(0, 1);
        assert!(p.choices[0][1].in_use && !p.choices[0][0].in_use);
        assert_eq!(p.handle(Action::Back), Some(Effect::Close));
    }

    #[test]
    fn the_same_picture_at_two_sizes_has_one_fingerprint() {
        let encode = |image: image::RgbImage| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            bytes.into_inner()
        };
        // A diagonal gradient, then the same picture at half size.
        let big = image::RgbImage::from_fn(600, 900, |x, y| {
            let v = ((x + y) * 255 / 1500) as u8;
            image::Rgb([v, v / 2, 255 - v])
        });
        let small = image::imageops::resize(&big, 300, 450, image::imageops::FilterType::Triangle);
        let flipped = image::imageops::flip_horizontal(&big);
        let (a, b, c) = (
            picture_hash(&encode(big)),
            picture_hash(&encode(small)),
            picture_hash(&encode(flipped)),
        );
        assert!(
            same_picture(a, b),
            "{a:064b}
{b:064b}"
        );
        assert!(!same_picture(a, c), "a different picture");
        assert_ne!(picture_hash(b"not an image"), picture_hash(b"also not one"));
    }
}
