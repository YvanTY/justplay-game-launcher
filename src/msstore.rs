//! The Microsoft Store's public catalog (no account, no key): Xbox and PC games, many of which
//! Steam never sold, with portrait covers, 16:9 key art and 4K screenshots.

use serde_json::Value;

use crate::{net, steam};

const SEARCH: &str = "https://displaycatalog.mp.microsoft.com/v7.0/productFamilies/autosuggest?market=US&languages=en-us&productFamilyNames=Games&query=";
const PRODUCTS: &str =
    "https://displaycatalog.mp.microsoft.com/v7.0/products?market=US&languages=en-us&bigIds=";

pub struct Product {
    pub id: String,
    pub title: String,
}

/// Full-size image addresses for one product, best first.
#[derive(Default)]
pub struct Art {
    /// Portrait covers (2:3 poster, then key art).
    pub covers: Vec<String>,
    /// 16:9 key art without, then with, the title.
    pub backgrounds: Vec<String>,
    pub screenshots: Vec<String>,
}

/// Games the store search returns for `title` (not music, apps or add-ons), most relevant first.
pub fn search(title: &str) -> Result<Vec<Product>, net::Error> {
    let body = net::get(&format!(
        "{SEARCH}{}",
        net::encode(&net::search_words(title))
    ))?;
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected Microsoft Store answer: {e}")))?;
    let mut products = Vec::new();
    for family in json["Results"].as_array().into_iter().flatten() {
        for product in family["Products"].as_array().into_iter().flatten() {
            let (Some(id), Some(name)) = (product["ProductId"].as_str(), product["Title"].as_str())
            else {
                continue;
            };
            if product["Type"].as_str() == Some("Game") && !steam::is_add_on(name) {
                products.push(Product {
                    id: id.to_string(),
                    title: name.to_string(),
                });
            }
        }
    }
    Ok(products)
}

/// The product that is clearly this game (same rules as Steam), if any.
pub fn find(title: &str) -> Result<Option<String>, net::Error> {
    let products = search(title)?;
    Ok(
        steam::best_title(title, products.iter().map(|p| p.title.as_str()))
            .map(|i| products[i].id.clone()),
    )
}

pub fn art(id: &str) -> Result<Art, net::Error> {
    let body = net::get(&format!("{PRODUCTS}{}", net::encode(id)))?;
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| net::Error::Failed(format!("unexpected Microsoft Store answer: {e}")))?;
    let images = json["Products"][0]["LocalizedProperties"][0]["Images"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let of = |purpose: &str| -> Vec<String> {
        images
            .iter()
            .filter(|image| image["ImagePurpose"].as_str() == Some(purpose))
            .filter_map(|image| image["Uri"].as_str())
            .map(|uri| {
                if uri.starts_with("//") {
                    format!("https:{uri}")
                } else {
                    uri.to_string()
                }
            })
            .filter(|uri| uri.starts_with("https://"))
            .collect()
    };
    let mut art = Art {
        covers: of("Poster"),
        backgrounds: of("SuperHeroArt"),
        screenshots: of("Screenshot"),
    };
    art.covers.extend(of("BrandedKeyArt"));
    art.backgrounds.extend(of("TitledHeroArt"));
    Ok(art)
}

/// The store's image server resizes on request: a light version for previews.
pub fn sized(url: &str, width: u32) -> String {
    format!("{url}?w={width}&q=85&format=jpg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_ask_the_server_for_a_smaller_picture() {
        assert_eq!(
            sized("https://store-images.s-microsoft.com/image/apps.1.2", 300),
            "https://store-images.s-microsoft.com/image/apps.1.2?w=300&q=85&format=jpg"
        );
    }

    /// Talks to the Microsoft Store: `cargo test msstore_live -- --ignored`.
    #[test]
    #[ignore]
    fn msstore_live() {
        let id = find("eFootball PES 2021")
            .unwrap()
            .expect("found as SEASON UPDATE");
        let art = art(&id).unwrap();
        assert!(!art.covers.is_empty() && !art.backgrounds.is_empty());
        assert!(art.screenshots.len() >= 3);
        assert_eq!(
            find("Blur").unwrap(),
            None,
            "songs named Blurry are not the game"
        );
    }
}
