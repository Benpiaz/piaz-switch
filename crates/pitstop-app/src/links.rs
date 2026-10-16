//! Where the window and the tray point people: the project's site, its
//! source and its author. The window asks for a link by name; only these
//! addresses are ever opened.

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::model::UiError;

pub const AUTHOR: &str = "Ben Piaz";

/// Name, address; the tray lists them in this order.
pub const LINKS: [(&str, &str); 4] = [
    ("website", "https://piaz-switch.vercel.app"),
    ("repo", "https://github.com/Benpiaz/piaz-switch"),
    ("youtube", "https://www.youtube.com/@benpiaz"),
    ("x", "https://x.com/benpiazai"),
];

pub fn url(name: &str) -> Option<&'static str> {
    LINKS.iter().find(|(n, _)| *n == name).map(|(_, url)| *url)
}

pub fn open(app: &AppHandle, name: &str) -> Result<(), UiError> {
    let url = url(name).ok_or_else(|| UiError::new("open", format!("No link named {name}.")))?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| UiError::new("open", e.to_string()))
}

/// Open one of the project's links, by name.
#[tauri::command]
pub fn open_link(app: AppHandle, name: String) -> Result<(), UiError> {
    open(&app, &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_is_https() {
        for (name, url) in LINKS {
            assert!(url.starts_with("https://"), "{name}");
        }
    }

    #[test]
    fn every_link_has_a_tray_label() {
        for (name, _) in LINKS {
            for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Vi] {
                assert!(!lang.link(name).is_empty(), "{name} in {lang:?}");
            }
        }
    }

    #[test]
    fn only_named_links_open() {
        assert_eq!(url("youtube"), Some("https://www.youtube.com/@benpiaz"));
        assert_eq!(url("x"), Some("https://x.com/benpiazai"));
        assert_eq!(url("https://example.com"), None);
        assert_eq!(url(""), None);
    }
}
