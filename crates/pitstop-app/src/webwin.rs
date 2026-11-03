//! A window on claude.ai or chatgpt.com for each Claude Code or Codex
//! account, each with a web profile of its own, so each stays signed in to
//! its own account on the site. Signing in there is separate from the tool's
//! login: the user does it once in each account's window, and the profile
//! keeps it: in `~/.pitstop/web/<tool>/<account>` on Windows and Linux (see
//! [`web_profile`]), in a data store of the app's named by who the account
//! is on macOS.
//!
//! The page is the site itself, so it gets nothing of the app: the window's
//! capability covers `main` only, and Tauri refuses every command from a
//! remote page no capability names. Links to anywhere but the site and the
//! usual sign-in pages open in the default browser.

use std::time::Duration;

use pitstop_core::state::AccountMeta;
use pitstop_core::{web_profile, ProviderId};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::app;
use crate::model::UiError;

/// What a tool's account window shows: the site's address and the domains
/// it may move between, besides the sign-in pages every site may use.
pub struct Site {
    pub host: &'static str,
    pub url: &'static str,
    pub domains: &'static [&'static str],
}

const CLAUDE: Site = Site {
    host: "claude.ai",
    url: "https://claude.ai/",
    domains: &["claude.ai", "claude.com", "anthropic.com"],
};

const CHATGPT: Site = Site {
    host: "chatgpt.com",
    url: "https://chatgpt.com/",
    domains: &["chatgpt.com", "openai.com"],
};

/// Pages a sign-in on either site may pass through: Google, Apple and
/// Microsoft accounts.
const SIGN_IN: &[&str] = &[
    "accounts.google.com",
    "appleid.apple.com",
    "idmsa.apple.com",
    "login.microsoftonline.com",
    "login.live.com",
    "account.live.com",
];

/// The site a tool's accounts get a window on; none for the editors.
pub fn site(provider: ProviderId) -> Option<&'static Site> {
    match provider {
        ProviderId::ClaudeCode => Some(&CLAUDE),
        ProviderId::Codex => Some(&CHATGPT),
        _ => None,
    }
}

/// The window's label: `web:<tool>:<account>`. Account names hold only
/// letters, digits, `-` and `_`; anything else is made `_`, as a label
/// can't hold it.
pub fn label(provider: ProviderId, name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("web:{}:{name}", provider.as_str())
}

/// Whether `host` is `domain` or one of its subdomains.
fn under(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// Whether the window may go to `url` itself: the site, its own domains and
/// the sign-in pages, over https; and blank pages. Anything else opens in
/// the default browser.
pub fn stays_inside(site: &Site, url: &Url) -> bool {
    match url.scheme() {
        "about" => true,
        "https" => url.host_str().is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            site.domains
                .iter()
                .chain(SIGN_IN)
                .any(|domain| under(&host, domain))
        }),
        _ => false,
    }
}

/// Whether the window hears of every frame's loads, not only the page's own.
/// WebKit on macOS asks about each frame and the webview layer doesn't say
/// which one asked; WebView2 on Windows, and WebKitGTK as used here, ask
/// only about the page itself.
const EVERY_FRAME: bool = cfg!(target_os = "macos");

/// Whether the window may load `url`. Where only the page's own navigations
/// are heard, one off the site goes to the browser. Where every frame's are
/// (`every_frame`), that would tear a claude.ai artifact, a Cloudflare check
/// or a payment form out of the page and into the browser, one tab each: so
/// there every https load stays, and only new windows go out.
pub fn may_load(site: &Site, url: &Url, every_frame: bool) -> bool {
    if every_frame {
        matches!(url.scheme(), "https" | "about" | "blob" | "data")
    } else {
        stays_inside(site, url)
    }
}

/// The 16 bytes that name an account's web profile on macOS, the same on
/// every run and version: two 64-bit FNV-1a hashes of the tool and
/// [`web_profile::store_key`], which a rename doesn't change.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn store_id(provider: ProviderId, store_key: &str) -> [u8; 16] {
    fn fnv(seed: u64, bytes: &[u8]) -> u64 {
        bytes.iter().fold(seed, |hash, &b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let key = format!("piaz-switch-web|{}|{store_key}", provider.as_str());
    let first = fnv(0xcbf2_9ce4_8422_2325, key.as_bytes());
    let second = fnv(first ^ 0x9e37_79b9_7f4a_7c15, key.as_bytes());
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&first.to_be_bytes());
    id[8..].copy_from_slice(&second.to_be_bytes());
    id
}

/// A separate profile per window needs macOS 14 or later; older ones open
/// the site in the default browser instead.
#[cfg(target_os = "macos")]
fn profiles_supported() -> bool {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
        })
        .is_some_and(|major| major >= 14)
}

#[cfg(not(target_os = "macos"))]
fn profiles_supported() -> bool {
    true
}

/// Send `url` to the default browser, when it is a web address.
fn to_browser(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "https" | "http") {
        let _ = app.opener().open_url(url.as_str(), None::<&str>);
    }
}

/// How an account's window opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Opened {
    /// Its own window, opened or brought forward.
    Window,
    /// The default browser, where a window of its own isn't possible.
    Browser,
}

/// Open `name`'s window on its tool's site, or bring it forward.
pub fn open(app: &AppHandle, provider: ProviderId, name: &str) -> Result<Opened, UiError> {
    let site = site(provider).ok_or_else(|| {
        UiError::new(
            "webUnsupported",
            format!("{} accounts have no web window.", provider.display_name()),
        )
    })?;
    let state = app::engine().state().map_err(|e| UiError::from(&e))?;
    let Some(account) = state
        .accounts
        .iter()
        .find(|a| a.provider == provider && a.name == name)
    else {
        return Err(UiError::from(&pitstop_core::Error::UnknownAccount(
            name.to_owned(),
        )));
    };
    let label = label(provider, name);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(Opened::Window);
    }
    let start: Url = site.url.parse().expect("a site's address parses");
    if !profiles_supported() {
        to_browser(app, &start);
        return Ok(Opened::Browser);
    }

    let leaving = app.clone();
    let popping = app.clone();
    let builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(start))
        .title(format!("{} — {name}", site.host))
        .inner_size(1180.0, 820.0)
        .min_inner_size(520.0, 420.0)
        .center()
        .on_navigation(move |url| {
            let inside = may_load(site, url, EVERY_FRAME);
            if !inside {
                to_browser(&leaving, url);
            }
            inside
        })
        .on_new_window(move |url, _features| {
            // A sign-in popup on the site's own pages keeps the profile;
            // any other new window is a link, for the browser.
            if stays_inside(site, &url) {
                NewWindowResponse::Allow
            } else {
                to_browser(&popping, &url);
                NewWindowResponse::Deny
            }
        });
    #[cfg(target_os = "macos")]
    let builder =
        builder.data_store_identifier(store_id(provider, &web_profile::store_key(account)));
    #[cfg(not(target_os = "macos"))]
    let builder = {
        let _ = account;
        let dir = web_profile::dir(&pitstop_core::Host::home(&app::host()), provider, name);
        std::fs::create_dir_all(&dir).map_err(|e| UiError::new("webWindow", e.to_string()))?;
        builder.data_directory(dir)
    };
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let window = builder
        .build()
        .map_err(|e| UiError::new("webWindow", e.to_string()))?;
    let _ = window.set_focus();
    Ok(Opened::Window)
}

/// Close `name`'s window, if open, and wait a moment for its browser to let
/// go of the profile's files.
fn close(app: &AppHandle, provider: ProviderId, name: &str) -> bool {
    match app.get_webview_window(&label(provider, name)) {
        Some(window) => {
            let _ = window.destroy();
            true
        }
        None => false,
    }
}

/// Try `work` a few times while a closed window's browser still holds the
/// profile's files. Says whether it worked.
fn retry<E>(mut work: impl FnMut() -> Result<(), E>, waited: bool) -> bool {
    let tries = if waited { 10 } else { 1 };
    for i in 0..tries {
        if work().is_ok() {
            return true;
        }
        if i + 1 < tries {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    false
}

/// Delete a data store of the app's on macOS, from any thread but the main
/// one (the deletion runs there, and this waits for it).
#[cfg(target_os = "macos")]
fn remove_store(app: &AppHandle, id: [u8; 16]) -> bool {
    tauri::async_runtime::block_on(app.remove_data_store(id)).is_ok()
}

/// An account renamed (`account` as it is now): its window closes and its
/// profile follows it to the new name. On macOS the profile is named by who
/// the account is, so it stays; where that isn't known it was named by the
/// old name, and goes: the site asks to sign in again. Elsewhere a folder
/// that can't be moved is left as an orphan, deleted at the next start, and
/// the new name starts a fresh profile.
pub fn follow_rename(app: &AppHandle, provider: ProviderId, old: &str, account: &AccountMeta) {
    if site(provider).is_none() {
        return;
    }
    let waited = close(app, provider, old);
    #[cfg(target_os = "macos")]
    {
        let _ = waited;
        if account.identity.is_none() {
            let gone = AccountMeta {
                name: old.to_owned(),
                ..account.clone()
            };
            let id = store_id(provider, &web_profile::store_key(&gone));
            retry(|| remove_store(app, id).then_some(()).ok_or(()), true);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let home = pitstop_core::Host::home(&app::host());
        retry(
            || web_profile::rename(&home, provider, old, &account.name),
            waited,
        );
    }
}

/// An account forgotten (`account` as it was): its window closes and its
/// profile goes, with the site's sign-in in it. One whose files a closing
/// window still holds is deleted at the next start, as an orphan.
pub fn follow_forget(app: &AppHandle, provider: ProviderId, account: &AccountMeta) {
    if site(provider).is_none() {
        return;
    }
    let waited = close(app, provider, &account.name);
    #[cfg(target_os = "macos")]
    {
        let id = store_id(provider, &web_profile::store_key(account));
        retry(|| remove_store(app, id).then_some(()).ok_or(()), waited);
    }
    #[cfg(not(target_os = "macos"))]
    {
        retry(
            || {
                web_profile::forget(
                    &pitstop_core::Host::home(&app::host()),
                    provider,
                    &account.name,
                )
            },
            waited,
        );
    }
}

/// At start, off the main thread: the profiles of accounts that no longer
/// exist go (a forget whose window still held the files, an account removed
/// on the command line). On macOS every data store of the app's is a web
/// profile; one no account names goes.
pub fn sweep(app: &AppHandle) {
    let Ok(state) = app::engine().state() else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let keep: Vec<[u8; 16]> = state
            .accounts
            .iter()
            .filter(|a| site(a.provider).is_some())
            .map(|a| store_id(a.provider, &web_profile::store_key(a)))
            .collect();
        if let Ok(ids) = tauri::async_runtime::block_on(app.fetch_data_store_identifiers()) {
            for id in ids.into_iter().filter(|id| !keep.contains(id)) {
                remove_store(app, id);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        web_profile::sweep(&pitstop_core::Host::home(&app::host()), &state);
    }
}

/// Open an account's window on its tool's site, or bring it forward. Says
/// whether it opened in the app or in the default browser.
#[tauri::command]
pub async fn open_web_window(
    app: AppHandle,
    provider: String,
    name: String,
) -> Result<Opened, UiError> {
    let pid = ProviderId::parse(&provider).map_err(|e| UiError::from(&e))?;
    tauri::async_runtime::spawn_blocking(move || open(&app, pid, &name))
        .await
        .map_err(|e| UiError::new("webWindow", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().unwrap()
    }

    #[test]
    fn only_claude_code_and_codex_accounts_get_a_site() {
        assert_eq!(site(ProviderId::ClaudeCode).unwrap().host, "claude.ai");
        assert_eq!(site(ProviderId::Codex).unwrap().host, "chatgpt.com");
        assert!(site(ProviderId::Cursor).is_none());
        assert!(site(ProviderId::Antigravity).is_none());
    }

    #[test]
    fn a_label_names_the_tool_and_the_account() {
        assert_eq!(
            label(ProviderId::ClaudeCode, "ben-1_x"),
            "web:claude-code:ben-1_x"
        );
        assert_eq!(label(ProviderId::Codex, "a b/c"), "web:codex:a_b_c");
    }

    #[test]
    fn each_account_keeps_its_profile_in_its_own_folder() {
        let home = std::path::Path::new("/home/ben");
        assert_eq!(
            web_profile::dir(home, ProviderId::ClaudeCode, "work"),
            home.join(".pitstop")
                .join("web")
                .join("claude-code")
                .join("work")
        );
        assert_ne!(
            web_profile::dir(home, ProviderId::Codex, "work"),
            web_profile::dir(home, ProviderId::ClaudeCode, "work")
        );
    }

    #[test]
    fn where_every_frame_is_heard_a_frame_in_the_page_stays_in_it() {
        let claude = site(ProviderId::ClaudeCode).unwrap();
        for frame in [
            "https://www.claudeusercontent.com/artifact",
            "https://challenges.cloudflare.com/turnstile",
            "https://js.stripe.com/v3/",
            "blob:https://claude.ai/1234",
            "about:blank",
        ] {
            assert!(may_load(claude, &url(frame), true), "{frame}");
        }
        // Nothing but the web's own addresses, even so.
        assert!(!may_load(
            claude,
            &url("mailto:support@anthropic.com"),
            true
        ));
        assert!(!may_load(claude, &url("http://claude.ai/"), true));
        // Where only the page's own navigations are heard, off the site goes out.
        assert!(!may_load(claude, &url("https://js.stripe.com/v3/"), false));
        assert!(may_load(claude, &url("https://claude.ai/new"), false));
    }

    #[test]
    fn the_site_and_its_sign_in_pages_stay_inside_and_the_rest_goes_out() {
        let claude = site(ProviderId::ClaudeCode).unwrap();
        for inside in [
            "https://claude.ai/new",
            "https://www.anthropic.com/legal",
            "https://platform.claude.com/",
            "https://accounts.google.com/o/oauth2/v2/auth?x=1",
            "https://appleid.apple.com/auth",
            "about:blank",
        ] {
            assert!(stays_inside(claude, &url(inside)), "{inside}");
        }
        for outside in [
            "https://chatgpt.com/",
            "https://github.com/anthropics",
            "https://evilclaude.ai/",
            "https://claude.ai.evil.com/",
            "http://claude.ai/",
            "mailto:support@anthropic.com",
            "https://checkout.stripe.com/pay",
        ] {
            assert!(!stays_inside(claude, &url(outside)), "{outside}");
        }
        let chatgpt = site(ProviderId::Codex).unwrap();
        assert!(stays_inside(
            chatgpt,
            &url("https://auth.openai.com/log-in")
        ));
        assert!(stays_inside(
            chatgpt,
            &url("https://login.live.com/oauth20")
        ));
        assert!(!stays_inside(chatgpt, &url("https://claude.ai/")));
    }

    #[test]
    fn a_macos_profile_id_is_stable_and_differs_per_account() {
        let work = store_id(ProviderId::ClaudeCode, "id|uuid-a:org-1");
        assert_eq!(work, store_id(ProviderId::ClaudeCode, "id|uuid-a:org-1"));
        assert_ne!(work, store_id(ProviderId::ClaudeCode, "id|uuid-b:org-1"));
        assert_ne!(work, store_id(ProviderId::Codex, "id|uuid-a:org-1"));
        // Fixed, so a new version finds the profiles an old one made.
        let hex: String = work.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), 32);
        assert_eq!(hex, "7c020bcc54454733fee50a05e4186cd6");
    }
}
