//! Grok Build, xAI's coding agent, which T3 Code runs as its `grok` provider.
//!
//! Its login is `~/.grok/auth.json` (verified on a real install, 2026-11): an
//! object keyed by `<issuer>::<client id>`, each value holding the session
//! (`key`, an access token; `refresh_token`; `expires_at`; `email`; `user_id`;
//! `team_id`). The vault blob is the file verbatim. The login names its owner,
//! so no request is needed to tell accounts apart.
//!
//! Grok renews the access token itself and rewrites the file; a switch is for
//! the sessions that start afterwards. Usage is the weekly pool its own
//! `/usage` reads (see `usage::parse_grok`).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::error::{Error, Result};
use crate::host::Host;
use crate::usage::parse_rfc3339;

pub struct Grok;

pub fn auth_path(host: &dyn Host) -> PathBuf {
    host.home().join(".grok/auth.json")
}

/// The signed-in session inside an `auth.json`: the first entry with a key.
fn session(blob: &[u8]) -> Option<Value> {
    let all: Value = serde_json::from_slice(blob).ok()?;
    all.as_object()?
        .values()
        .find(|entry| text(entry, "key").is_some())
        .cloned()
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    let found = value.get(key)?.as_str()?.trim();
    (!found.is_empty()).then_some(found)
}

/// The access token inside a login, unless its expiry is already past.
/// Deliberately not `Debug`.
pub enum Access {
    Valid(String),
    Expired,
    Missing,
}

pub fn access(blob: &[u8], now: i64) -> Access {
    let Some(session) = session(blob) else {
        return Access::Missing;
    };
    let Some(key) = text(&session, "key") else {
        return Access::Missing;
    };
    match text(&session, "expires_at").and_then(parse_rfc3339) {
        Some(expiry) if expiry <= now => Access::Expired,
        _ => Access::Valid(key.to_owned()),
    }
}

impl Provider for Grok {
    fn id(&self) -> ProviderId {
        ProviderId::Grok
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        let Some(blob) = host.read(&auth_path(host))? else {
            return Ok(None);
        };
        if session(&blob).is_none() {
            return Ok(None);
        }
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        if session(blob).is_none() {
            return Err(Error::InvalidData("not a Grok login".into()));
        }
        host.write_atomic(&auth_path(host), blob)
    }

    /// A Grok account is known by its address.
    fn details(&self, blob: &[u8]) -> Details {
        let email = session(blob)
            .and_then(|s| text(&s, "email").map(str::to_owned))
            .filter(|e| e.contains('@'));
        Details {
            identity: email.as_ref().map(|e| format!("{}:", e.to_lowercase())),
            label: email,
            plan: None,
        }
    }

    /// Grok signs in by itself; Piaz Switch only parks what it left.
    fn read_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<Option<ActiveCredential>> {
        Ok(None)
    }

    fn discard_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    fn login(email: &str, expires_at: &str) -> String {
        format!(
            r#"{{"https://auth.x.ai::c1":{{"key":"tok-{email}","email":"{email}","expires_at":"{expires_at}","refresh_token":"r"}}}}"#
        )
    }

    #[test]
    fn the_login_names_its_owner_and_is_switched_by_file() {
        let host = FakeHost::new();
        host.put_file(auth_path(&host), login("A@x.com", "2099-01-01T00:00:00Z"));
        let live = Grok.read_active(&host).unwrap().unwrap();
        assert_eq!(live.details.label.as_deref(), Some("A@x.com"));
        assert_eq!(live.details.identity.as_deref(), Some("a@x.com:"));

        let other = login("b@x.com", "2099-01-01T00:00:00Z");
        Grok.write_active(&host, other.as_bytes()).unwrap();
        assert_eq!(host.file(&auth_path(&host)).unwrap(), other.as_bytes());
    }

    #[test]
    fn an_expired_token_is_not_offered_and_a_file_with_no_session_is_not_a_login() {
        let expired = login("a@x.com", "2001-01-01T00:00:00.123456789Z");
        assert!(matches!(
            access(expired.as_bytes(), 1_800_000_000),
            Access::Expired
        ));
        let live = login("a@x.com", "2099-01-01T00:00:00.5Z");
        assert!(matches!(
            access(live.as_bytes(), 1_800_000_000),
            Access::Valid(_)
        ));
        let host = FakeHost::new();
        host.put_file(auth_path(&host), "{}");
        assert!(Grok.read_active(&host).unwrap().is_none());
        assert!(Grok.write_active(&host, b"{}").is_err());
    }
}
