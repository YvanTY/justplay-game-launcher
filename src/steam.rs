//! Game art from Steam's public store data (no account, no key): find the game by title, then
//! download its library images from the store's image server.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::library::find_image;
use crate::net;

const SEARCH: &str = "https://store.steampowered.com/api/storesearch/?l=english&cc=US&term=";
const DETAILS: &str = "https://store.steampowered.com/api/appdetails?filters=screenshots&appids=";
const IMAGES: &str = "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps";

/// Suffixes that name the same game in another edition, so "God of War" still matches
/// "God of War Remastered" and "Grand Theft Auto V" matches "… V Enhanced". Normalized.
const EDITIONS: &[&str] = &[
    "remastered",
    "remaster",
    "hd",
    "hd remaster",
    "definitive edition",
    "game of the year edition",
    "game of the year",
    "goty",
    "goty edition",
    "complete edition",
    "deluxe edition",
    "enhanced",
    "enhanced edition",
    "legacy",
    "legacy edition",
    "directors cut",
    "anniversary edition",
    "ultimate edition",
    "gold edition",
    "special edition",
    "standard edition",
    "season update",
];

/// Words that mark add-ons and other products sold under the game's name.
const NOT_THE_GAME: &[&str] = &[
    "pass",
    "pack",
    "dlc",
    "soundtrack",
    "ost",
    "upgrade",
    "bundle",
    "demo",
    "costume",
    "skin",
    "artbook",
    "expansion",
    "character",
    "kit",
    "coins",
    "points",
    "currency",
    "season",
];

/// Art folder slot, then the Steam images to try for it, sharpest first.
pub const SLOTS: [(&str, &[&str]); 3] = [
    ("icon", &["library_600x900_2x.jpg", "library_600x900.jpg"]),
    ("hero", &["library_hero_2x.jpg", "library_hero.jpg"]),
    ("logo", &["logo_2x.png", "logo.png"]),
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

/// The Steam app for this title, if the store lists one that is clearly the same game.
/// Games Steam no longer sells don't appear here (see `pcgw`).
pub fn find(title: &str) -> Result<Option<u32>, net::Error> {
    let apps = search(title)?;
    Ok(best_title(title, apps.iter().map(|(_, name)| name.as_str())).map(|index| apps[index].0))
}

/// Every app the store search returns for `title`, most relevant first: (app id, name).
pub fn search(title: &str) -> Result<Vec<(u32, String)>, net::Error> {
    let body = net::get(&format!("{SEARCH}{}", net::encode(title)))?;
    let search: Search = serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected Steam answer: {e}")))?;
    Ok(search
        .items
        .into_iter()
        .filter(|i| i.kind.is_empty() || i.kind == "app")
        .map(|i| (i.id, i.name))
        .collect())
}

/// Soundtracks, DLC, passes: products sold under a game's name that aren't the game.
pub fn is_add_on(name: &str) -> bool {
    normalize(name)
        .split(' ')
        .any(|word| NOT_THE_GAME.contains(&word))
}

/// The Steam images for a slot ("icon", "hero", "logo"), sharpest first.
pub fn image_urls(app: u32, slot: &str) -> Vec<String> {
    SLOTS
        .iter()
        .filter(|(name, _)| *name == slot)
        .flat_map(|(_, sources)| sources.iter())
        .map(|source| format!("{IMAGES}/{app}/{source}"))
        .collect()
}

/// The store banner (460x215), shown as a cover when nothing better exists.
pub fn banner_url(app: u32) -> String {
    format!("{IMAGES}/{app}/header.jpg")
}

/// Index of the name that is the same game as `title`: an exact match first, then another
/// edition, then (for long titles) the title with a subtitle added. A near miss is rejected,
/// because wrong art is worse than none. Ties go to the earlier name (search relevance), except
/// between two different subtitles, which is a guess (the art picker settles those).
pub fn best_title<'a>(title: &str, names: impl Iterator<Item = &'a str>) -> Option<usize> {
    let wanted = normalize(title);
    let mut ranked: Vec<(u8, usize)> = names
        .enumerate()
        .filter_map(|(index, name)| match_rank(&wanted, &normalize(name)).map(|rank| (rank, index)))
        .collect();
    ranked.sort();
    match ranked.as_slice() {
        [(2, _), (2, _), ..] => None,
        [(_, index), ..] => Some(*index),
        [] => None,
    }
}

fn match_rank(wanted: &str, name: &str) -> Option<u8> {
    if wanted == name {
        return Some(0);
    }
    if same_but_edition(wanted, name) {
        return Some(1);
    }
    with_subtitle(wanted, name).then_some(2)
}

fn same_but_edition(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.strip_prefix(short)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|edition| EDITIONS.contains(&edition))
}

/// "Demon Slayer Kimetsu no Yaiba" is "Demon Slayer Kimetsu no Yaiba The Hinokami Chronicles",
/// but "God of War" is not "God of War Ragnarok": only titles of four words or more may gain a
/// subtitle, and never a number (a sequel) or an add-on word.
fn with_subtitle(wanted: &str, name: &str) -> bool {
    if wanted.split(' ').count() < 4 {
        return false;
    }
    let Some(rest) = name.strip_prefix(wanted).and_then(|r| r.strip_prefix(' ')) else {
        return false;
    };
    rest.split(' ')
        .all(|word| !word.chars().all(|c| c.is_ascii_digit()) && !NOT_THE_GAME.contains(&word))
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

/// Downloads the library images for the slots in `wanted` ("icon", "hero", "logo") into `dir`,
/// skipping slots that already have an image. Returns how many were saved; a slot Steam doesn't
/// have is skipped.
pub fn download(app: u32, dir: &Path, wanted: &[&str]) -> Result<usize, net::Error> {
    let mut saved = 0;
    for (slot, sources) in SLOTS.iter().filter(|(slot, _)| wanted.contains(slot)) {
        if find_image(dir, slot).is_some() {
            continue;
        }
        for source in *sources {
            if save(&format!("{IMAGES}/{app}/{source}"), dir, slot)? {
                saved += 1;
                break;
            }
        }
    }
    Ok(saved)
}

/// Older games have no library background; a full-size store screenshot stands in for it.
pub fn download_screenshot(app: u32, dir: &Path) -> Result<bool, net::Error> {
    match screenshot_urls(app)?.into_iter().next() {
        Some(url) => save(&url, dir, "hero"),
        None => Ok(false),
    }
}

/// Full-size store screenshots (1920x1080), in store order; empty when the store page is gone.
pub fn screenshot_urls(app: u32) -> Result<Vec<String>, net::Error> {
    Ok(screenshots(app)?.into_iter().map(|s| s.full).collect())
}

/// A store screenshot: a light 600x338 version for previews and the full 1920x1080 one.
pub struct Shot {
    pub thumbnail: String,
    pub full: String,
}

pub fn screenshots(app: u32) -> Result<Vec<Shot>, net::Error> {
    #[derive(Deserialize)]
    struct Details {
        #[serde(default)]
        success: bool,
        data: Option<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(default)]
        screenshots: Vec<Screenshot>,
    }
    #[derive(Deserialize)]
    struct Screenshot {
        path_thumbnail: String,
        path_full: String,
    }
    let body = net::get(&format!("{DETAILS}{app}"))?;
    let details: std::collections::HashMap<String, Details> =
        serde_json::from_slice(&body).unwrap_or_default();
    Ok(details
        .into_values()
        .find(|d| d.success)
        .and_then(|d| d.data)
        .map(|d| {
            d.screenshots
                .into_iter()
                .map(|s| Shot {
                    thumbnail: s.path_thumbnail,
                    full: s.path_full,
                })
                .collect()
        })
        .unwrap_or_default())
}

/// The small store banner, kept for games Steam has no cover for.
pub fn download_banner(app: u32, dir: &Path) -> Result<bool, net::Error> {
    save(&banner_url(app), dir, "icon")
}

/// Saves the image at `url` as `<name>.<jpg|png>` in `dir`. Ok(false) when the server has no
/// such image; never replaces a file that is already there.
pub fn save(url: &str, dir: &Path, name: &str) -> Result<bool, net::Error> {
    let bytes = match net::get(url) {
        Ok(bytes) => bytes,
        Err(net::Error::Status(_)) => return Ok(false),
        Err(e) => return Err(e),
    };
    let extension = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        return Ok(false);
    };
    if find_image(dir, name).is_some() {
        return Ok(false);
    }
    fs::create_dir_all(dir).map_err(|e| net::Error::Failed(format!("{}: {e}", dir.display())))?;
    let target = dir.join(format!("{name}.{extension}"));
    // Written aside then renamed, so a half-downloaded file is never shown.
    let temp = target.with_extension("part");
    fs::write(&temp, &bytes)
        .and_then(|()| fs::rename(&temp, &target))
        .map_err(|e| net::Error::Failed(format!("{}: {e}", target.display())))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(title: &str, names: &[&str]) -> Option<usize> {
        best_title(title, names.iter().copied())
    }

    #[test]
    fn exact_titles_match_despite_trademarks_and_numerals() {
        let names = [
            "The Last of Us™ Part I - Upgrade to Digital Deluxe Edition",
            "The Last of Us™ Part I",
        ];
        assert_eq!(best("The Last of Us Part 1", &names), Some(1));
        assert_eq!(
            best("God of War", &["God of War", "God of War Ragnarök"]),
            Some(0)
        );
        assert_eq!(
            best("PRO EVOLUTION SOCCER 2019", &["Pro Evolution Soccer 2019"]),
            Some(0)
        );
    }

    #[test]
    fn add_ons_are_recognised() {
        assert!(is_add_on("God of War Soundtrack"));
        assert!(is_add_on(
            "Demon Slayer -Kimetsu no Yaiba- The Hinokami Chronicles: Character Pass"
        ));
        assert!(!is_add_on("Grand Theft Auto V Enhanced"));
        assert_eq!(image_urls(10, "logo").len(), 2);
        assert!(image_urls(10, "icon")[0].ends_with("/10/library_600x900_2x.jpg"));
    }

    #[test]
    fn near_misses_are_rejected() {
        // Steam's only answer for this PS3 game is a different game.
        assert_eq!(best("Dante's Inferno", &["Dantes Bullet Inferno"]), None);
        assert_eq!(best("God of War", &["God of War Soundtrack"]), None);
        assert_eq!(
            best("God of War", &["God of War Ragnarök"]),
            None,
            "short titles stay exact"
        );
        assert_eq!(
            best(
                "Blur",
                &["Ricochet Blur", "Synth Riders: Blur - \"Song 2\""]
            ),
            None
        );
    }

    #[test]
    fn editions_of_the_same_game_match() {
        assert_eq!(
            best("Sleeping Dogs", &["Sleeping Dogs: Definitive Edition"]),
            Some(0)
        );
        assert_eq!(
            best("Fallout 3: Game of the Year Edition", &["Fallout 3"]),
            Some(0)
        );
        let gta = [
            "Grand Theft Auto V Enhanced",
            "Grand Theft Auto: Vice City – The Definitive Edition",
        ];
        assert_eq!(best("Grand Theft Auto V", &gta), Some(0));
        assert_eq!(
            best("eFootball PES 2021", &["EFootball PES 2021 Season Update"]),
            Some(0)
        );
    }

    #[test]
    fn long_titles_may_gain_a_subtitle_but_not_a_sequel_or_an_add_on() {
        // Steam's own order for this search.
        let names = [
            "Demon Slayer -Kimetsu no Yaiba- The Hinokami Chronicles 2",
            "Demon Slayer -Kimetsu no Yaiba- The Hinokami Chronicles",
            "Demon Slayer -Kimetsu no Yaiba- Sweep the Board!",
            "Demon Slayer -Kimetsu no Yaiba- The Hinokami Chronicles: Character Pass",
        ];
        assert_eq!(
            best("Demon Slayer - Kimetsu no Yaiba", &names[..2]),
            Some(1)
        );
        // Two different subtitles fit: guessing could show another game's art.
        assert_eq!(best("Demon Slayer - Kimetsu no Yaiba", &names), None);
        assert_eq!(
            best(
                "Grand Theft Auto V",
                &["Grand Theft Auto V Enhanced", "Grand Theft Auto V Legacy"]
            ),
            Some(0),
            "editions are not guesses"
        );
    }

    /// Talks to Steam: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_lookup_and_download() {
        assert_eq!(find("God of War").unwrap(), Some(1593500));
        assert_eq!(find("The Last of Us Part I").unwrap(), Some(1888930));
        assert_eq!(find("Dante's Inferno").unwrap(), None);
        assert_eq!(find("Grand Theft Auto V").unwrap(), Some(3240220));
        let dir = std::env::temp_dir().join(format!("gl-steam-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let saved = download(1593500, &dir, &["icon", "hero", "logo"]).unwrap();
        assert_eq!(saved, 3);
        let hero = image::open(dir.join("hero.jpg")).unwrap();
        assert_eq!(
            (hero.width(), hero.height()),
            (3840, 1240),
            "the sharp background"
        );
        assert_eq!(
            download(1593500, &dir, &["icon"]).unwrap(),
            0,
            "never re-downloads"
        );
        // Blur left the store long ago: no library art, but screenshots and a banner remain.
        let old = dir.join("blur");
        assert_eq!(download(42640, &old, &["icon", "hero", "logo"]).unwrap(), 0);
        assert!(download_screenshot(42640, &old).unwrap());
        assert!(download_banner(42640, &old).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }
}
