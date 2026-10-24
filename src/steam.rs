//! Game art from Steam's public store data (no account, no key): find the game by title, then
//! download its library images from the store's image server.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::net;

const SEARCH: &str = "https://store.steampowered.com/api/storesearch/?l=english&cc=US&term=";
const IMAGES: &str = "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps";

/// Suffixes that name the same game in another edition, so "God of War" still matches
/// "God of War Remastered". Normalized (see `normalize`).
const EDITIONS: &[&str] = &[
    "remastered",
    "definitive edition",
    "game of the year edition",
    "game of the year",
    "goty",
    "goty edition",
    "complete edition",
    "deluxe edition",
    "enhanced edition",
    "directors cut",
    "anniversary edition",
    "ultimate edition",
    "gold edition",
    "special edition",
    "standard edition",
];

/// File in the art folder, then the Steam images to try for it, sharpest first.
pub const SLOTS: [(&str, &[&str]); 3] = [
    (
        "icon.jpg",
        &["library_600x900_2x.jpg", "library_600x900.jpg"],
    ),
    ("hero.jpg", &["library_hero_2x.jpg", "library_hero.jpg"]),
    ("logo.png", &["logo_2x.png", "logo.png"]),
];

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: u32,
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
}

/// The Steam app with exactly this title, if any. A near miss is rejected: wrong art is worse
/// than none.
pub fn find(title: &str) -> Result<Option<u32>, net::Error> {
    let body = net::get(&format!("{SEARCH}{}", net::encode(title)))?;
    let search: Search = serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected Steam answer: {e}")))?;
    Ok(best_match(title, &search.items))
}

fn best_match(title: &str, items: &[Item]) -> Option<u32> {
    let wanted = normalize(title);
    let apps = || {
        items
            .iter()
            .filter(|i| i.kind.is_empty() || i.kind == "app")
    };
    apps()
        .find(|i| normalize(&i.name) == wanted)
        .or_else(|| apps().find(|i| same_but_edition(&wanted, &normalize(&i.name))))
        .map(|i| i.id)
}

fn same_but_edition(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.strip_prefix(short)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|edition| EDITIONS.contains(&edition))
}

/// Lowercase words without punctuation or trademark signs, roman numerals as digits:
/// "The Last of Us™ Part I" and "the last of us part 1" compare equal.
fn normalize(title: &str) -> String {
    let cleaned: String = title
        .replace('&', " and ")
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©' | '\''))
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .map(|word| match word {
            "i" => "1",
            "ii" => "2",
            "iii" => "3",
            "iv" => "4",
            "v" => "5",
            "vi" => "6",
            "vii" => "7",
            "viii" => "8",
            "ix" => "9",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Downloads the images named in `wanted` (file names from SLOTS) into `dir`, never replacing a
/// file that is already there. Returns how many were saved; a slot Steam doesn't have is skipped.
pub fn download(app: u32, dir: &Path, wanted: &[&str]) -> Result<usize, net::Error> {
    let mut saved = 0;
    for (file, sources) in SLOTS.iter().filter(|(file, _)| wanted.contains(file)) {
        let target = dir.join(file);
        if target.exists() {
            continue;
        }
        for source in *sources {
            match net::get(&format!("{IMAGES}/{app}/{source}")) {
                Ok(bytes) if looks_like_image(&bytes) => {
                    fs::create_dir_all(dir)
                        .map_err(|e| net::Error::Failed(format!("{}: {e}", dir.display())))?;
                    // Written aside then renamed, so a half-downloaded file is never shown.
                    let temp = target.with_extension("part");
                    fs::write(&temp, &bytes)
                        .and_then(|()| fs::rename(&temp, &target))
                        .map_err(|e| net::Error::Failed(format!("{}: {e}", target.display())))?;
                    saved += 1;
                    break;
                }
                Ok(_) | Err(net::Error::Status(_)) => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(saved)
}

fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[(u32, &str)]) -> Vec<Item> {
        names
            .iter()
            .map(|&(id, name)| Item {
                id,
                name: name.into(),
                kind: "app".into(),
            })
            .collect()
    }

    #[test]
    fn exact_titles_match_despite_trademarks_and_numerals() {
        let found = items(&[
            (
                2254450,
                "The Last of Us™ Part I - Upgrade to Digital Deluxe Edition",
            ),
            (1888930, "The Last of Us™ Part I"),
        ]);
        assert_eq!(best_match("The Last of Us Part 1", &found), Some(1888930));
        let gow = items(&[(1593500, "God of War"), (2322010, "God of War Ragnarök")]);
        assert_eq!(best_match("God of War", &gow), Some(1593500));
    }

    #[test]
    fn near_misses_are_rejected() {
        // Steam's only answer for this PS3 game is a different game.
        let found = items(&[(1239990, "Dantes Bullet Inferno")]);
        assert_eq!(best_match("Dante's Inferno", &found), None);
        let dlc = items(&[(1, "God of War Soundtrack")]);
        assert_eq!(best_match("God of War", &dlc), None);
    }

    #[test]
    fn editions_of_the_same_game_match() {
        let found = items(&[(1, "Sleeping Dogs: Definitive Edition")]);
        assert_eq!(best_match("Sleeping Dogs", &found), Some(1));
        let found = items(&[(2, "Fallout 3")]);
        assert_eq!(
            best_match("Fallout 3: Game of the Year Edition", &found),
            Some(2)
        );
    }

    /// Talks to Steam: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_lookup_and_download() {
        assert_eq!(find("God of War").unwrap(), Some(1593500));
        assert_eq!(find("The Last of Us Part I").unwrap(), Some(1888930));
        assert_eq!(find("Dante's Inferno").unwrap(), None);
        let dir = std::env::temp_dir().join(format!("gl-steam-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let saved = download(1593500, &dir, &["icon.jpg", "hero.jpg", "logo.png"]).unwrap();
        assert_eq!(saved, 3);
        let hero = image::open(dir.join("hero.jpg")).unwrap();
        assert_eq!(
            (hero.width(), hero.height()),
            (3840, 1240),
            "the sharp background"
        );
        assert_eq!(
            download(1593500, &dir, &["icon.jpg"]).unwrap(),
            0,
            "never re-downloads"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_real_images_are_saved() {
        assert!(looks_like_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(looks_like_image(b"\x89PNG\r\n\x1a\n...."));
        assert!(!looks_like_image(b"<html>not found</html>"));
    }
}
