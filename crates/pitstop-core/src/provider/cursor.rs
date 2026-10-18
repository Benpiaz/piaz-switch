//! Cursor, the editor.
//!
//! Its login is a few `cursorAuth/*` rows of its `state.vscdb` (see
//! [`crate::vscdb`]): the access and refresh tokens (JWTs whose `sub` is
//! `auth0|user_…`), the email and the plan it last saw. The vault blob is
//! those rows as a JSON object, key by key. A private sign-in (see
//! [`crate::signin`]) leaves the same object in `cursor-login.json`.
//!
//! Sources: the open-source Cursor switchers and usage readers (CodexBar,
//! OpenUsage, multi-cursor), checked against each other, 2026-10.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::editor;
use crate::error::{Error, Result};
use crate::host::Host;

pub const ACCESS_TOKEN: &str = "cursorAuth/accessToken";
pub const REFRESH_TOKEN: &str = "cursorAuth/refreshToken";
pub const EMAIL: &str = "cursorAuth/cachedEmail";
pub const SIGN_UP_TYPE: &str = "cursorAuth/cachedSignUpType";
pub const MEMBERSHIP: &str = "cursorAuth/stripeMembershipType";
pub const SUBSCRIPTION: &str = "cursorAuth/stripeSubscriptionStatus";
pub const PROFILE: &str = "cursorAuth/cachedScopedProfile";
pub const AUTH_ID: &str = "cursorAuth/authId";

/// Every row of a login, in the order they are read.
pub const KEYS: [&str; 8] = [
    ACCESS_TOKEN,
    REFRESH_TOKEN,
    EMAIL,
    SIGN_UP_TYPE,
    MEMBERSHIP,
    SUBSCRIPTION,
    PROFILE,
    AUTH_ID,
];

/// What a private sign-in leaves in its directory.
pub const SIGNED_IN_FILE: &str = "cursor-login.json";

pub struct Cursor;

fn rows_of(blob: &[u8]) -> Option<BTreeMap<String, String>> {
    serde_json::from_slice(blob).ok()
}

/// The rows as a blob, when they hold a login at all.
fn blob_of(rows: BTreeMap<String, String>) -> Option<Vec<u8>> {
    let signed_in = rows.get(ACCESS_TOKEN).is_some_and(|t| !t.trim().is_empty());
    signed_in.then(|| serde_json::to_vec(&rows).unwrap_or_default())
}

/// The access token of a blob and when it lapses (epoch seconds).
pub fn access_token(blob: &[u8]) -> Option<(String, Option<i64>)> {
    let token = rows_of(blob)?.remove(ACCESS_TOKEN)?;
    let expires = super::codex::jwt_claims(&token).and_then(|c| c.get("exp")?.as_i64());
    Some((token, expires))
}

/// `auth0|user_01…` → `user_01…`, the id Cursor's site knows the user by.
fn user_id(sub: &str) -> &str {
    sub.rsplit('|').next().unwrap_or(sub)
}

/// The plan as Cursor names it on its site.
fn plan_name(membership: &str) -> String {
    match membership.trim().to_lowercase().as_str() {
        "free" => "Free".into(),
        "free_trial" => "Free trial".into(),
        "pro" => "Pro".into(),
        "pro_plus" | "pro+" => "Pro+".into(),
        "ultra" => "Ultra".into(),
        "business" | "team" | "teams" => "Teams".into(),
        "enterprise" => "Enterprise".into(),
        other => super::capitalized(other),
    }
}

impl Cursor {
    fn db(host: &dyn Host) -> PathBuf {
        editor::app(ProviderId::Cursor)
            .expect("Cursor is an editor")
            .state_db(host)
    }

    fn read_from(&self, host: &dyn Host, db: &Path) -> Result<Option<ActiveCredential>> {
        let Some(rows) = host.vscdb_get(db, &KEYS)? else {
            return Ok(None);
        };
        Ok(blob_of(rows).map(|blob| ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }
}

impl Provider for Cursor {
    fn id(&self) -> ProviderId {
        ProviderId::Cursor
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        self.read_from(host, &Self::db(host))
    }

    /// Only while Cursor is closed (see [`crate::editor`]): refused while it
    /// is open. A row the login lacks goes, so nothing of the previous
    /// account stays (a refresh token above all); how they signed up stays.
    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        if host.editor_running(ProviderId::Cursor) {
            return Err(Error::EditorStillOpen("Cursor".into()));
        }
        let rows = rows_of(blob)
            .filter(|rows| rows.contains_key(ACCESS_TOKEN))
            .ok_or_else(|| Error::InvalidData("not a Cursor login".into()))?;
        let rows: BTreeMap<String, String> = rows
            .into_iter()
            .filter(|(k, _)| KEYS.contains(&k.as_str()))
            .collect();
        let stale: Vec<&str> = KEYS
            .into_iter()
            .filter(|k| *k != SIGN_UP_TYPE && !rows.contains_key(*k))
            .collect();
        host.vscdb_put(&Self::db(host), &rows, &stale)
    }

    /// The email from the rows; the user id from the token's `sub`, which
    /// stays when the email changes; the plan Cursor last saw.
    fn details(&self, blob: &[u8]) -> Details {
        let Some(rows) = rows_of(blob) else {
            return Details::default();
        };
        let sub = rows
            .get(ACCESS_TOKEN)
            .and_then(|t| super::codex::jwt_claims(t))
            .and_then(|c| super::text(&c, "sub"))
            .or_else(|| rows.get(AUTH_ID).cloned());
        Details {
            label: rows
                .get(EMAIL)
                .map(|e| e.trim().to_owned())
                .filter(|e| !e.is_empty()),
            identity: sub.map(|s| format!("{}:", user_id(&s))),
            plan: rows
                .get(MEMBERSHIP)
                .filter(|m| !m.is_empty())
                .map(|m| plan_name(m)),
        }
    }

    fn read_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<ActiveCredential>> {
        let Some(bytes) = host.read(&dir.join(SIGNED_IN_FILE))? else {
            return Ok(None);
        };
        Ok(rows_of(&bytes)
            .and_then(blob_of)
            .map(|blob| ActiveCredential {
                details: self.details(&blob),
                blob,
            }))
    }

    /// The sign-in writes nothing outside its directory.
    fn discard_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use base64::Engine as _;

    pub fn fake_jwt(payload: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.sig", b64(r#"{"alg":"HS256"}"#), b64(payload))
    }

    pub fn signed_in(host: &FakeHost, sub: &str, email: &str, plan: &str) {
        let token = fake_jwt(&format!(r#"{{"sub":"{sub}","exp":4102444800}}"#));
        host.put_vscdb(
            Cursor::db(host),
            &[
                (ACCESS_TOKEN, &token),
                (REFRESH_TOKEN, &token),
                (EMAIL, email),
                (MEMBERSHIP, plan),
                (SIGN_UP_TYPE, "Auth_0"),
                ("telemetry.machineId", "untouched"),
            ],
        );
    }

    #[test]
    fn a_login_is_its_rows_and_says_who_and_which_plan() {
        let host = FakeHost::new();
        signed_in(&host, "auth0|user_01ABC", "a@x.com", "pro");
        let active = Cursor.read_active(&host).unwrap().unwrap();
        assert_eq!(
            active.details,
            Details {
                label: Some("a@x.com".into()),
                identity: Some("user_01ABC:".into()),
                plan: Some("Pro".into()),
            }
        );
        let rows = rows_of(&active.blob).unwrap();
        assert!(!rows.contains_key("telemetry.machineId"));
        assert_eq!(rows.len(), 5);
        let (_, exp) = access_token(&active.blob).unwrap();
        assert_eq!(exp, Some(4_102_444_800));
    }

    #[test]
    fn signed_out_or_never_opened_is_no_login() {
        let host = FakeHost::new();
        assert!(Cursor.read_active(&host).unwrap().is_none());
        host.put_vscdb(Cursor::db(&host), &[(ACCESS_TOKEN, ""), (EMAIL, "a@x.com")]);
        assert!(Cursor.read_active(&host).unwrap().is_none());
    }

    #[test]
    fn writing_a_login_replaces_the_account_rows_and_nothing_else() {
        let host = FakeHost::new();
        signed_in(&host, "auth0|user_A", "a@x.com", "pro");
        let token = fake_jwt(r#"{"sub":"auth0|user_B"}"#);
        let other = serde_json::to_vec(&BTreeMap::from([
            (ACCESS_TOKEN, token.as_str()),
            (REFRESH_TOKEN, token.as_str()),
            (EMAIL, "b@x.com"),
        ]))
        .unwrap();
        Cursor.write_active(&host, &other).unwrap();
        let db = host.vscdb(&Cursor::db(&host)).unwrap();
        assert_eq!(db[EMAIL], "b@x.com");
        assert_eq!(db[ACCESS_TOKEN], token);
        // B's plan isn't known: A's must not stay
        assert!(!db.contains_key(MEMBERSHIP));
        // nor A's refresh token beside B's access token
        let alone = serde_json::to_vec(&BTreeMap::from([(ACCESS_TOKEN, token.as_str())])).unwrap();
        Cursor.write_active(&host, &alone).unwrap();
        assert!(!host
            .vscdb(&Cursor::db(&host))
            .unwrap()
            .contains_key(REFRESH_TOKEN));
        assert_eq!(db[SIGN_UP_TYPE], "Auth_0");
        assert_eq!(db["telemetry.machineId"], "untouched");
        assert!(Cursor.write_active(&host, b"{}").is_err());
    }

    #[test]
    fn a_login_is_never_written_under_an_open_cursor() {
        let host = FakeHost::new();
        signed_in(&host, "auth0|user_A", "a@x.com", "pro");
        let before = host.vscdb(&Cursor::db(&host));
        host.set_editor_open(ProviderId::Cursor, true);
        let token = fake_jwt(r#"{"sub":"auth0|user_B"}"#);
        let other = serde_json::to_vec(&BTreeMap::from([(ACCESS_TOKEN, token.as_str())])).unwrap();
        assert!(matches!(
            Cursor.write_active(&host, &other),
            Err(Error::EditorStillOpen(_))
        ));
        assert_eq!(host.vscdb(&Cursor::db(&host)), before);
    }

    #[test]
    fn a_private_sign_in_is_read_from_its_file() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-c");
        let token = fake_jwt(r#"{"sub":"auth0|user_N"}"#);
        host.put_file(
            dir.join(SIGNED_IN_FILE),
            format!(
                r#"{{"{ACCESS_TOKEN}":"{token}","{REFRESH_TOKEN}":"{token}","{EMAIL}":"n@x.com"}}"#
            ),
        );
        let got = Cursor.read_signed_in(&host, dir).unwrap().unwrap();
        assert_eq!(got.details.label.as_deref(), Some("n@x.com"));
        assert_eq!(got.details.identity.as_deref(), Some("user_N:"));
    }

    #[test]
    fn plans_read_as_the_site_names_them() {
        assert_eq!(plan_name("pro_plus"), "Pro+");
        assert_eq!(plan_name("free_trial"), "Free trial");
        assert_eq!(plan_name("ultra"), "Ultra");
        assert_eq!(plan_name("something"), "Something");
    }
}
