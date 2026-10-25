//! Is a newer JustPlay out? Asks GitHub for the latest release (public API, no account) at most
//! once a day; Settings then offers its download page.

use serde::{Deserialize, Serialize};

use crate::net;

pub const RELEASES_API: &str =
    "https://api.github.com/repos/YvanTY/justplay-game-launcher/releases/latest";
/// How often to ask (seconds).
pub const CHECK_EVERY: u64 = 24 * 60 * 60;

/// A newer release: its version ("0.3.0") and the page to download it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub version: String,
    pub page: String,
}

/// The latest release on GitHub, newer or not.
pub fn latest() -> Result<Update, String> {
    let body = net::get(RELEASES_API).map_err(|e| format!("update check: {e}"))?;
    parse_release(&body)
}

fn parse_release(body: &[u8]) -> Result<Update, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        html_url: String,
    }
    let release: Release =
        serde_json::from_slice(body).map_err(|e| format!("update check: {e}"))?;
    Ok(Update {
        version: release.tag_name.trim_start_matches('v').to_string(),
        page: release.html_url,
    })
}

/// Is `candidate` ("0.3.0" or "v0.3.0") a later version than `current`? Anything unreadable
/// is not newer, so a strange tag never nags.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (numbers(candidate), numbers(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn numbers(version: &str) -> Option<[u32; 3]> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = match parts.next() {
            Some(part) => part.parse().ok()?,
            None => 0,
        };
    }
    parts.next().is_none().then_some(out)
}

/// Opens a web page in the default browser.
pub fn open_page(page: &str) -> Result<(), String> {
    if !page.starts_with("https://") {
        return Err(format!("not a web page: {page}"));
    }
    std::process::Command::new("explorer")
        .arg(page)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open the browser: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.5"), "10 is more than 9");
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("nightly", "0.2.0"), "unreadable tags never nag");
        assert!(!is_newer("0.3.0-beta", "0.2.0"));
    }

    #[test]
    fn reads_githubs_release_answer() {
        let body = br#"{"tag_name":"v0.3.0","html_url":"https://github.com/YvanTY/justplay-game-launcher/releases/tag/v0.3.0","assets":[]}"#;
        assert_eq!(
            parse_release(body),
            Ok(Update {
                version: "0.3.0".into(),
                page: "https://github.com/YvanTY/justplay-game-launcher/releases/tag/v0.3.0".into(),
            })
        );
        assert!(parse_release(b"<html>rate limited</html>").is_err());
    }

    #[test]
    fn only_web_pages_are_opened() {
        assert!(open_page(r"C:\Windows\System32\cmd.exe").is_err());
    }

    /// `cargo test latest_release_on_github -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn latest_release_on_github() {
        let update = latest().unwrap();
        println!("{update:?}");
        assert!(!update.version.is_empty());
    }
}
