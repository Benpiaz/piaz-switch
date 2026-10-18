//! Antigravity, Google's editor (Gemini, and other makers' models, on a
//! Google account).
//!
//! Its login is a few rows of its `state.vscdb` (see [`crate::vscdb`]),
//! each base64 of a protobuf "topic" of `{sentinel key, row}` entries:
//! - `antigravityUnifiedStateSync.oauthToken`: the Google tokens, under
//!   `oauthTokenInfoSentinelKey` (field 1 access token, 3 refresh token,
//!   4 expiry), beside the account's project context;
//! - `antigravityUnifiedStateSync.userStatus`: who it is (field 3 name,
//!   7 email) and the quota of every model as Antigravity last read it
//!   (field 33);
//! - the account's credits, enterprise project and picture.
//!
//! The vault blob is those rows as a JSON object, verbatim. Pitstop reads
//! them and puts them back; it never calls Google itself, so the quota shown
//! is the one Antigravity last saw for that account.
//!
//! Format checked on a real install, 2026-10 (and against the open-source
//! Antigravity tools that read it).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::editor;
use crate::error::{Error, Result};
use crate::host::Host;
use crate::pb;
use crate::usage::{Usage, Window};

pub const OAUTH_TOKEN: &str = "antigravityUnifiedStateSync.oauthToken";
pub const USER_STATUS: &str = "antigravityUnifiedStateSync.userStatus";
const ENTERPRISE: &str = "antigravityUnifiedStateSync.enterprisePreferences";
const CREDITS: &str = "antigravityUnifiedStateSync.modelCredits";
const PICTURE: &str = "antigravity.profileUrl";
/// The format before 2026, read by older builds; never kept, always removed.
const LEGACY: &str = "jetskiStateSync.agentManagerInitState";
const ONBOARDED: &str = "antigravityOnboarding";

/// Every row of a login.
pub const KEYS: [&str; 5] = [OAUTH_TOKEN, USER_STATUS, ENTERPRISE, CREDITS, PICTURE];

const TOKEN_SENTINEL: &str = "oauthTokenInfoSentinelKey";
const STATUS_SENTINEL: &str = "userStatusSentinelKey";

pub struct Antigravity;

fn rows_of(blob: &[u8]) -> Option<BTreeMap<String, String>> {
    serde_json::from_slice(blob).ok()
}

/// The payload under `sentinel` in a topic row: base64 of a topic whose
/// entries are `{1: key, 2: {1: base64 payload}}`.
fn payload(row: &str, sentinel: &str) -> Option<Vec<u8>> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let topic = b64.decode(row.trim()).ok()?;
    pb::repeated(&topic, 1).into_iter().find_map(|entry| {
        let entry = entry.bytes()?;
        if pb::field(entry, 1)?.text()? != sentinel {
            return None;
        }
        let row = pb::field(entry, 2)?.bytes()?;
        b64.decode(pb::field(row, 1)?.bytes()?).ok()
    })
}

/// Whether the rows hold a Google login that can be renewed.
fn signed_in(rows: &BTreeMap<String, String>) -> bool {
    rows.get(OAUTH_TOKEN)
        .and_then(|row| payload(row, TOKEN_SENTINEL))
        .and_then(|token| pb::field(&token, 3)?.text().map(|r| !r.is_empty()))
        .unwrap_or(false)
}

fn status(rows: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    payload(rows.get(USER_STATUS)?, STATUS_SENTINEL)
}

fn email(rows: &BTreeMap<String, String>) -> Option<String> {
    let status = status(rows)?;
    let email = pb::field(&status, 7)?.text()?.trim();
    email.contains('@').then(|| email.to_owned())
}

/// `Gemini 3.1 Pro (High)` → `Gemini 3.1 Pro`.
fn base_name(label: &str) -> &str {
    match label.find(" (") {
        Some(at) if label.ends_with(')') => &label[..at],
        _ => label,
    }
}

/// The pool every model that isn't Gemini draws on (Claude, GPT-OSS).
pub const OTHER_MODELS: &str = "Claude & GPT";

/// The quota Antigravity last saw for the account, one window per pool, under
/// a name that stays the same whatever it reads (accounts are compared by
/// it): each Gemini model by its name, its variants (High, Low) together;
/// every other maker's model in one shared pool. A pool shows its fullest
/// model.
pub fn quota(blob: &[u8]) -> Option<Usage> {
    let rows = rows_of(blob)?;
    let status = status(&rows)?;
    let config = pb::field(&status, 33)?.bytes()?;
    let mut windows: Vec<Window> = Vec::new();
    for model in pb::repeated(config, 1) {
        let Some(model) = model.bytes() else { continue };
        let Some(label) = pb::field(model, 1).and_then(pb::Wire::text) else {
            continue;
        };
        let Some(quota) = pb::field(model, 15).and_then(pb::Wire::bytes) else {
            continue;
        };
        let resets_at = pb::field(quota, 2)
            .and_then(pb::Wire::bytes)
            .and_then(|t| pb::field(t, 1)?.varint())
            .and_then(|s| i64::try_from(s).ok())
            .filter(|&s| s > 0);
        // A used-up model carries no fraction left, only when it resets.
        let left = match pb::field(quota, 1).and_then(pb::Wire::float) {
            Some(f) if f.is_finite() => f64::from(f).clamp(0.0, 1.0),
            Some(_) => continue,
            None if resets_at.is_some() => 0.0,
            None => continue,
        };
        let percent = ((1.0 - left) * 1000.0).round() / 10.0;
        let base = base_name(label);
        let pool = if base.starts_with("Gemini") {
            base
        } else {
            OTHER_MODELS
        };
        let reading = Window {
            label: pool.to_owned(),
            percent,
            resets_at,
            length_secs: None,
            quiet: false,
        };
        match windows.iter_mut().find(|w| w.label == pool) {
            Some(w) if reading.percent > w.percent => *w = reading,
            Some(_) => {}
            None => windows.push(reading),
        }
    }
    Some(Usage { windows })
}

impl Antigravity {
    fn db(host: &dyn Host) -> PathBuf {
        editor::app(ProviderId::Antigravity)
            .expect("Antigravity is an editor")
            .state_db(host)
    }

    fn read_from(&self, host: &dyn Host, db: &Path) -> Result<Option<ActiveCredential>> {
        let Some(rows) = host.vscdb_get(db, &KEYS)? else {
            return Ok(None);
        };
        if !signed_in(&rows) {
            return Ok(None);
        }
        let blob = serde_json::to_vec(&rows).unwrap_or_default();
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }
}

impl Provider for Antigravity {
    fn id(&self) -> ProviderId {
        ProviderId::Antigravity
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        self.read_from(host, &Self::db(host))
    }

    /// Only while Antigravity is closed: see [`crate::editor`]. The rows go
    /// back as they were parked; rows of the previous account the parked
    /// login lacks go, and so does the old format's.
    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        if host.editor_running(ProviderId::Antigravity) {
            return Err(Error::EditorStillOpen("Antigravity".into()));
        }
        let rows: BTreeMap<String, String> = rows_of(blob)
            .filter(signed_in)
            .ok_or_else(|| Error::InvalidData("not an Antigravity login".into()))?
            .into_iter()
            .filter(|(k, _)| KEYS.contains(&k.as_str()))
            .collect();
        let mut remove: Vec<&str> = KEYS
            .into_iter()
            .filter(|k| !rows.contains_key(*k))
            .collect();
        remove.push(LEGACY);
        let mut set = rows;
        set.insert(ONBOARDED.into(), "true".into());
        host.vscdb_put(&Self::db(host), &set, &remove)
    }

    /// A Google account is known by its address.
    fn details(&self, blob: &[u8]) -> Details {
        let email = rows_of(blob).as_ref().and_then(email);
        Details {
            identity: email.as_ref().map(|e| format!("{}:", e.to_lowercase())),
            label: email,
            plan: None,
        }
    }

    /// A private sign-in runs Antigravity with `dir` as its data directory.
    fn read_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<ActiveCredential>> {
        self.read_from(host, &editor::App::state_db_in(dir))
    }

    fn discard_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pb::write;
    use crate::testing::FakeHost;

    fn topic(sentinel: &str, payload: &[u8]) -> String {
        let b64 = base64::engine::general_purpose::STANDARD;
        let row = write::bytes(1, b64.encode(payload).as_bytes());
        let entry = [write::bytes(1, sentinel.as_bytes()), write::bytes(2, &row)].concat();
        b64.encode(write::bytes(1, &entry))
    }

    pub fn token_row(refresh: &str) -> String {
        let info = [
            write::bytes(1, b"ya29.access"),
            write::bytes(2, b"Bearer"),
            write::bytes(3, refresh.as_bytes()),
            write::bytes(4, &write::uint(1, 1_800_000_000)),
        ]
        .concat();
        topic(TOKEN_SENTINEL, &info)
    }

    fn model(label: &str, left: Option<f32>, reset: u64) -> Vec<u8> {
        let mut quota = write::bytes(2, &write::uint(1, reset));
        if let Some(left) = left {
            quota = [write::float(1, left), quota].concat();
        }
        write::bytes(
            1,
            &[write::bytes(1, label.as_bytes()), write::bytes(15, &quota)].concat(),
        )
    }

    pub fn status_row(email: &str) -> String {
        let models = [
            model("Gemini 3.1 Pro (High)", Some(0.8), 1_900_000_000),
            model("Gemini 3.1 Pro (Low)", Some(0.8), 1_900_000_000),
            model("Gemini 3 Flash", None, 1_900_000_500),
            model("Claude Sonnet 4.6 (Thinking)", Some(0.25), 1_900_001_000),
            model("Claude Opus 4.6 (Thinking)", Some(0.25), 1_900_001_000),
            model("GPT-OSS 120B (Medium)", Some(0.25), 1_900_001_000),
        ]
        .concat();
        let status = [
            write::bytes(3, b"Ben"),
            write::bytes(7, email.as_bytes()),
            write::bytes(33, &models),
        ]
        .concat();
        topic(STATUS_SENTINEL, &status)
    }

    fn signed_in_as(host: &FakeHost, email: &str) {
        host.put_vscdb(
            Antigravity::db(host),
            &[
                (OAUTH_TOKEN, &token_row("1//refresh")),
                (USER_STATUS, &status_row(email)),
                (PICTURE, "https://lh3.example/a"),
                ("antigravityUnifiedStateSync.theme", "dark"),
            ],
        );
    }

    #[test]
    fn a_login_is_its_rows_and_is_known_by_its_address() {
        let host = FakeHost::new();
        signed_in_as(&host, "Ben@Gmail.com");
        let active = Antigravity.read_active(&host).unwrap().unwrap();
        assert_eq!(active.details.label.as_deref(), Some("Ben@Gmail.com"));
        assert_eq!(active.details.identity.as_deref(), Some("ben@gmail.com:"));
        let rows = rows_of(&active.blob).unwrap();
        assert_eq!(rows.len(), 3, "only the login's rows: {:?}", rows.keys());
    }

    #[test]
    fn without_a_refresh_token_there_is_no_login() {
        let host = FakeHost::new();
        assert!(Antigravity.read_active(&host).unwrap().is_none());
        host.put_vscdb(Antigravity::db(&host), &[(OAUTH_TOKEN, &token_row(""))]);
        assert!(Antigravity.read_active(&host).unwrap().is_none());
        host.put_vscdb(Antigravity::db(&host), &[(OAUTH_TOKEN, "not base64!")]);
        assert!(Antigravity.read_active(&host).unwrap().is_none());
    }

    #[test]
    fn writing_a_login_puts_its_rows_back_and_drops_the_previous_accounts() {
        let host = FakeHost::new();
        signed_in_as(&host, "a@gmail.com");
        host.put_vscdb(
            Antigravity::db(&host),
            &[
                (OAUTH_TOKEN, &token_row("1//a")),
                (USER_STATUS, &status_row("a@gmail.com")),
                (PICTURE, "https://lh3.example/a"),
                (LEGACY, "old"),
                ("antigravityUnifiedStateSync.theme", "dark"),
            ],
        );
        let b: BTreeMap<&str, String> = BTreeMap::from([
            (OAUTH_TOKEN, token_row("1//b")),
            (USER_STATUS, status_row("b@gmail.com")),
        ]);
        Antigravity
            .write_active(&host, &serde_json::to_vec(&b).unwrap())
            .unwrap();
        let db = host.vscdb(&Antigravity::db(&host)).unwrap();
        assert_eq!(db[OAUTH_TOKEN], token_row("1//b"));
        assert!(!db.contains_key(PICTURE), "a's picture stayed");
        assert!(!db.contains_key(LEGACY));
        assert_eq!(db[ONBOARDED], "true");
        assert_eq!(db["antigravityUnifiedStateSync.theme"], "dark");
        assert!(Antigravity.write_active(&host, b"{}").is_err());
    }

    #[test]
    fn the_quota_shows_each_gemini_model_and_the_shared_pool() {
        let blob =
            serde_json::to_vec(&BTreeMap::from([(USER_STATUS, status_row("a@x.com"))])).unwrap();
        let usage = quota(&blob).unwrap();
        let shown: Vec<(&str, f64, Option<i64>)> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.percent, w.resets_at))
            .collect();
        assert_eq!(
            shown,
            [
                ("Gemini 3.1 Pro", 20.0, Some(1_900_000_000)),
                // used up: no fraction left, only when it comes back
                ("Gemini 3 Flash", 100.0, Some(1_900_000_500)),
                ("Claude & GPT", 75.0, Some(1_900_001_000)),
            ]
        );
        assert!(quota(b"{}").is_none());
    }

    #[test]
    fn a_pool_keeps_its_name_and_shows_its_fullest_model() {
        let models = [
            model("Gemini 3.1 Pro (High)", Some(0.5), 10),
            model("Gemini 3.1 Pro (Low)", Some(0.9), 20),
            model("GPT-OSS 120B (Medium)", Some(0.9), 30),
            model("Claude Opus 4.6 (Thinking)", Some(0.4), 40),
        ]
        .concat();
        let status = [write::bytes(7, b"a@x.com"), write::bytes(33, &models)].concat();
        let blob = serde_json::to_vec(&BTreeMap::from([(
            USER_STATUS,
            topic(STATUS_SENTINEL, &status),
        )]))
        .unwrap();
        let shown: Vec<(String, f64, Option<i64>)> = quota(&blob)
            .unwrap()
            .windows
            .into_iter()
            .map(|w| (w.label, w.percent, w.resets_at))
            .collect();
        assert_eq!(
            shown,
            [
                ("Gemini 3.1 Pro".to_owned(), 50.0, Some(10)),
                (OTHER_MODELS.to_owned(), 60.0, Some(40)),
            ]
        );
    }

    #[test]
    fn a_login_is_never_written_under_an_open_antigravity() {
        let host = FakeHost::new();
        signed_in_as(&host, "a@gmail.com");
        host.set_editor_open(ProviderId::Antigravity, true);
        let b = serde_json::to_vec(&BTreeMap::from([(OAUTH_TOKEN, token_row("1//b"))])).unwrap();
        assert!(matches!(
            Antigravity.write_active(&host, &b),
            Err(Error::EditorStillOpen(_))
        ));
    }

    #[test]
    fn a_private_sign_in_is_read_from_its_own_data_directory() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-ag");
        host.put_vscdb(
            editor::App::state_db_in(dir),
            &[
                (OAUTH_TOKEN, &token_row("1//new")),
                (USER_STATUS, &status_row("n@gmail.com")),
            ],
        );
        let got = Antigravity.read_signed_in(&host, dir).unwrap().unwrap();
        assert_eq!(got.details.label.as_deref(), Some("n@gmail.com"));
    }

    /// Against the Antigravity on this computer, read only: `cargo test -p
    /// pitstop-core real_antigravity -- --ignored --nocapture`. Prints the
    /// quota rows, never the address or a token.
    #[test]
    #[ignore = "reads the real Antigravity database"]
    fn real_antigravity_login_reads() {
        let host = crate::host::RealHost::new();
        let Some(login) = Antigravity.read_active(&host).unwrap() else {
            println!("Antigravity is signed out here");
            return;
        };
        println!("has an address: {}", login.details.label.is_some());
        for w in quota(&login.blob).unwrap().windows {
            println!("{} {}% resets {:?}", w.label, w.percent, w.resets_at);
        }
    }
}
