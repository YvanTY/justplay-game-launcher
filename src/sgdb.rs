//! SteamGridDB, the community art site Playnite uses: many covers, backgrounds and logos per
//! game. It needs a free personal API key, set in config.toml (`[art] steamgriddb_key`).

use serde_json::Value;

use crate::{net, steam};

const API: &str = "https://www.steamgriddb.com/api/v2";

pub struct Image {
    /// Full size.
    pub url: String,
    /// A light version for previews.
    pub thumb: String,
}

fn call(key: &str, path: &str) -> Result<Value, net::Error> {
    let body = net::get_with_header(
        &format!("{API}{path}"),
        Some(&format!("Authorization: Bearer {key}")),
    )?;
    serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected SteamGridDB answer: {e}")))
}

/// Games SteamGridDB knows by this name: (id, name), most relevant first.
pub fn search(key: &str, title: &str) -> Result<Vec<(u64, String)>, net::Error> {
    let words = net::search_words(title);
    let json = call(
        key,
        &format!("/search/autocomplete/{}", net::encode(&words)),
    )?;
    Ok(json["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|game| Some((game["id"].as_u64()?, game["name"].as_str()?.to_string())))
        .collect())
}

/// The game that is clearly this one (same rules as Steam), if any.
pub fn find(key: &str, title: &str) -> Result<Option<u64>, net::Error> {
    let games = search(key, title)?;
    Ok(steam::best_title(title, games.iter().map(|(_, name)| name.as_str())).map(|i| games[i].0))
}

/// `kind`: "grids" (covers), "heroes" (backgrounds) or "logos". Static images only.
pub fn images(key: &str, game: u64, kind: &str) -> Result<Vec<Image>, net::Error> {
    // Portrait covers only: the launcher's tiles are 2:3.
    let filter = if kind == "grids" {
        "?dimensions=600x900,342x482,660x930&types=static"
    } else {
        "?types=static"
    };
    let json = call(key, &format!("/{kind}/game/{game}{filter}"))?;
    Ok(json["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|image| {
            let url = image["url"].as_str()?;
            let thumb = image["thumb"].as_str().unwrap_or(url);
            (url.starts_with("https://") && thumb.starts_with("https://")).then(|| Image {
                url: url.to_string(),
                thumb: thumb.to_string(),
            })
        })
        .collect())
}
