//! Signing in to Cursor in the browser, as its own command line does, so the
//! login in the editor is never touched: a sign-in page bound to a fresh
//! PKCE pair, and a poll that hands over the tokens once the page approves.
//!
//! - `https://cursor.com/loginDeepControl?challenge=<S256>&uuid=<v4>&mode=login&redirectTarget=cli`
//! - `GET https://api2.cursor.sh/auth/poll?uuid=<v4>&verifier=<verifier>`:
//!   404 until approved, then `{accessToken, refreshToken, authId}`.
//!
//! The page approves whoever is signed in to cursor.com in that browser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use sha2::Digest as _;

use crate::provider::cursor;

const POLL_EVERY: Duration = Duration::from_secs(2);
const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);

/// A sign-in's secret and the public halves sent with it.
pub struct Pkce {
    verifier: String,
    challenge: String,
    uuid: String,
}

fn random<const N: usize>() -> [u8; N] {
    use aes_gcm::aead::Generate;
    <[u8; N]>::generate()
}

impl Pkce {
    pub fn new() -> Pkce {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let verifier = b64.encode(random::<32>());
        let challenge = b64.encode(sha2::Sha256::digest(verifier.as_bytes()));
        let mut id = random::<16>();
        id[6] = (id[6] & 0x0f) | 0x40;
        id[8] = (id[8] & 0x3f) | 0x80;
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        Pkce {
            verifier,
            challenge,
            uuid,
        }
    }

    /// The page to sign in on.
    pub fn login_url(&self) -> String {
        format!(
            "https://cursor.com/loginDeepControl?challenge={}&uuid={}&mode=login&redirectTarget=cli",
            self.challenge, self.uuid
        )
    }

    fn poll_url(&self) -> String {
        format!(
            "https://api2.cursor.sh/auth/poll?uuid={}&verifier={}",
            self.uuid, self.verifier
        )
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a sign-in stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Polled {
    Waiting,
    /// The login is in the directory, as [`cursor::SIGNED_IN_FILE`].
    Done,
    Failed(String),
}

/// The login rows a poll's answer gives, or None while it isn't there.
pub fn rows_from_answer(answer: &Value) -> Option<BTreeMap<String, String>> {
    let text = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let access = text("accessToken")?;
    let mut rows = BTreeMap::from([
        (
            cursor::REFRESH_TOKEN.to_owned(),
            text("refreshToken").unwrap_or_else(|| access.clone()),
        ),
        (cursor::ACCESS_TOKEN.to_owned(), access),
    ]);
    if let Some(id) = text("authId") {
        rows.insert(cursor::AUTH_ID.to_owned(), id);
    }
    Some(rows)
}

fn json(mut response: ureq::http::Response<ureq::Body>) -> Option<Value> {
    let text = response.body_mut().read_to_string().ok()?;
    serde_json::from_str(&text).ok()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// The account's address and plan, as Cursor's own site reads them; what
/// can't be read is left for Cursor to fill in when it opens.
fn describe(agent: &ureq::Agent, rows: &mut BTreeMap<String, String>) {
    let Some(token) = rows.get(cursor::ACCESS_TOKEN).cloned() else {
        return;
    };
    let me = agent
        .post("https://api2.cursor.sh/aiserver.v1.DashboardService/GetMe")
        .header("Authorization", format!("Bearer {token}"))
        .header("Connect-Protocol-Version", "1")
        .header("Content-Type", "application/json")
        .send("{}")
        .ok()
        .filter(|r| r.status() == 200)
        .and_then(json);
    if let Some(email) = me
        .as_ref()
        .and_then(|m| m.get("email")?.as_str())
        .filter(|e| e.contains('@'))
    {
        rows.insert(cursor::EMAIL.to_owned(), email.to_owned());
    }
    // Not `/auth/full_stripe_profile`: since the weekly plans (2026-10) its
    // `membershipType` says "free" for a Plus account. Cursor's own row is
    // left for Cursor to fill in.
    let info = agent
        .post(crate::usage::CURSOR_PLAN_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Connect-Protocol-Version", "1")
        .header("Content-Type", "application/json")
        .send("{}")
        .ok()
        .filter(|r| r.status() == 200)
        .and_then(json);
    if let Some(plan) = info
        .as_ref()
        .and_then(|i| i.pointer("/planInfo/planName")?.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        rows.insert(cursor::PLAN_NAME.to_owned(), plan.to_owned());
    }
}

/// Write the login where [`cursor::Cursor`] reads a private sign-in, unless
/// the sign-in was cancelled meanwhile: its directory is gone then, and must
/// not come back holding the tokens.
fn keep(dir: &Path, rows: &BTreeMap<String, String>, stop: &AtomicBool) -> Result<(), String> {
    use crate::host::{Host, RealHost};
    if stop.load(Ordering::SeqCst) || !dir.is_dir() {
        return Err("the sign-in was cancelled".into());
    }
    let bytes = serde_json::to_vec(rows).map_err(|e| e.to_string())?;
    RealHost::new()
        .write_atomic(&dir.join(cursor::SIGNED_IN_FILE), &bytes)
        .map_err(|e| e.to_string())
}

/// Poll for the login on a thread of its own until it comes, the time runs
/// out or `stop` is set; where it stands is in the returned cell.
pub fn poll(pkce: Pkce, dir: PathBuf, stop: Arc<AtomicBool>) -> Arc<Mutex<Polled>> {
    let state = Arc::new(Mutex::new(Polled::Waiting));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || {
        let set = |polled: Polled| {
            if let Ok(mut s) = shared.lock() {
                *s = polled;
            }
        };
        let agent = agent();
        let started = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            if started.elapsed() > GIVE_UP_AFTER {
                set(Polled::Failed(
                    "the sign-in page was not approved in time".into(),
                ));
                return;
            }
            let answer = agent
                .get(&pkce.poll_url())
                .call()
                .ok()
                .filter(|r| r.status() == 200)
                .and_then(json);
            if let Some(mut rows) = answer.as_ref().and_then(rows_from_answer) {
                describe(&agent, &mut rows);
                set(match keep(&dir, &rows, &stop) {
                    Ok(()) => Polled::Done,
                    Err(e) => Polled::Failed(format!("couldn't keep the login: {e}")),
                });
                return;
            }
            std::thread::sleep(POLL_EVERY);
        }
    });
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_sign_in_gets_its_own_pair_and_the_challenge_is_the_verifiers_hash() {
        let (a, b) = (Pkce::new(), Pkce::new());
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.uuid, b.uuid);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert_eq!(
            a.challenge,
            b64.encode(sha2::Sha256::digest(a.verifier.as_bytes()))
        );
        // a version 4 UUID
        assert_eq!(a.uuid.len(), 36);
        assert_eq!(&a.uuid[14..15], "4");
        let url = a.login_url();
        assert!(url.starts_with("https://cursor.com/loginDeepControl?"));
        assert!(url.contains(&a.uuid) && url.contains(&a.challenge));
        // the secret half never goes to the browser
        assert!(!url.contains(&a.verifier));
        assert!(a.poll_url().contains(&a.verifier));
    }

    #[test]
    fn an_answer_with_tokens_is_a_login_and_one_without_is_not_yet() {
        let rows = rows_from_answer(&serde_json::json!({
            "accessToken": "jwt-a", "refreshToken": "jwt-r", "authId": "auth0|user_1"
        }))
        .unwrap();
        assert_eq!(rows[cursor::ACCESS_TOKEN], "jwt-a");
        assert_eq!(rows[cursor::REFRESH_TOKEN], "jwt-r");
        assert_eq!(rows[cursor::AUTH_ID], "auth0|user_1");
        // the session token renews itself when no refresh token comes
        let alone = rows_from_answer(&serde_json::json!({"accessToken": "jwt-a"})).unwrap();
        assert_eq!(alone[cursor::REFRESH_TOKEN], "jwt-a");
        assert!(rows_from_answer(&serde_json::json!({"accessToken": ""})).is_none());
        assert!(rows_from_answer(&serde_json::json!({})).is_none());
    }
}
