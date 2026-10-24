//! PCGamingWiki's public pages (no account, no key), for games the Steam store search can't find:
//! it still knows the Steam app id of games Steam stopped selling (their art is still on Steam's
//! image server) and has box art for games that were never on Steam.

use serde_json::Value;

use crate::{net, steam};

const API: &str = "https://www.pcgamingwiki.com/w/api.php?format=json&";

#[derive(Debug, PartialEq)]
pub struct WikiGame {
    pub steam_app: Option<u32>,
    /// Address of the box art image.
    pub cover: Option<String>,
}

/// The wiki's page for this game: the page with exactly this title, else the search result that
/// is clearly the same game (same matching rules as Steam).
pub fn lookup(title: &str) -> Result<Option<WikiGame>, net::Error> {
    let text = match page_text(title)? {
        Some(text) => text,
        None => {
            let found = search(title)?;
            let Some(index) = steam::best_title(title, found.iter().map(String::as_str)) else {
                return Ok(None);
            };
            match page_text(&found[index])? {
                Some(text) => text,
                None => return Ok(None),
            }
        }
    };
    let steam_app = field(&text, "steam appid").and_then(|v| {
        v.split(|c: char| !c.is_ascii_digit())
            .find(|n| !n.is_empty())
            .and_then(|n| n.parse().ok())
    });
    let cover = match field(&text, "cover") {
        Some(file) if !file.is_empty() => image_url(&file)?,
        _ => None,
    };
    Ok(Some(WikiGame { steam_app, cover }))
}

fn api(query: &str) -> Result<Value, net::Error> {
    let body = net::get(&format!("{API}{query}"))?;
    serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected PCGamingWiki answer: {e}")))
}

/// The page's source text, following redirects; None when there is no such page.
fn page_text(title: &str) -> Result<Option<String>, net::Error> {
    let json = api(&format!(
        "action=query&prop=revisions&rvprop=content&rvslots=main&redirects=1&titles={}",
        net::encode(title)
    ))?;
    let Some(pages) = json["query"]["pages"].as_object() else {
        return Ok(None);
    };
    Ok(pages.values().find_map(|page| {
        page["revisions"][0]["slots"]["main"]["*"]
            .as_str()
            .map(str::to_string)
    }))
}

fn search(title: &str) -> Result<Vec<String>, net::Error> {
    let json = api(&format!(
        "action=query&list=search&srlimit=5&srsearch={}",
        net::encode(title)
    ))?;
    Ok(json["query"]["search"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|r| r["title"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

fn image_url(file: &str) -> Result<Option<String>, net::Error> {
    let json = api(&format!(
        "action=query&prop=imageinfo&iiprop=url&titles={}",
        net::encode(&format!("File:{file}"))
    ))?;
    let Some(pages) = json["query"]["pages"].as_object() else {
        return Ok(None);
    };
    Ok(pages.values().find_map(|page| {
        page["imageinfo"][0]["url"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .map(str::to_string)
    }))
}

/// A value from the page's game infobox: `|steam appid = 456610`.
fn field(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.trim_start().strip_prefix('|')?.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infobox_fields_are_read() {
        let text = "{{Infobox game\n|cover        = Pro Evolution Soccer 2017 Cover.jpg\n\
                    |steam appid  = 456610\n|steam appid side = 1, 2\n}}";
        assert_eq!(field(text, "steam appid").as_deref(), Some("456610"));
        assert_eq!(
            field(text, "cover").as_deref(),
            Some("Pro Evolution Soccer 2017 Cover.jpg")
        );
        assert_eq!(field(text, "gog id"), None);
    }

    /// Talks to PCGamingWiki: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_lookups() {
        let pes = lookup("Pro Evolution Soccer 2017").unwrap().unwrap();
        assert_eq!(pes.steam_app, Some(456610));
        let fifa = lookup("FIFA 19").unwrap().unwrap();
        assert_eq!(fifa.steam_app, None, "never on Steam");
        assert!(
            fifa.cover
                .unwrap()
                .starts_with("https://images.pcgamingwiki.com/")
        );
        // No page by this exact name; the search finds "eFootball PES 2021 Season Update".
        assert!(lookup("eFootball PES 2021").unwrap().is_some());
        assert_eq!(lookup("Surely No Game Has This Name 123").unwrap(), None);
    }
}
