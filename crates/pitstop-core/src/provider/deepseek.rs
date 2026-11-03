//! A DeepSeek API key, the way OpenCode keeps it (see [`crate::opencode_db`]).
//! The vault blob is the credential's JSON verbatim (`{"type":3,"key":"sk-…"}`).
//!
//! A key carries no owner, so an account is known by a hash of the key and
//! shown by its last characters. DeepSeek bills a balance, not a period: its
//! usage is the money left (see `usage::parse_deepseek`).
//!
//! OpenCode reads the key when a session starts: a switch is for the sessions
//! that start after it.

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::error::{Error, Result};
use crate::host::Host;

pub struct DeepSeek;

/// The API key inside a credential, trimmed.
pub fn api_key(blob: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(blob).ok()?;
    let key = value.get("key")?.as_str()?.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

/// `sk-…a1b2`: enough to tell two keys apart, nothing to use.
fn masked(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("sk-…{tail}")
}

impl Provider for DeepSeek {
    fn id(&self) -> ProviderId {
        ProviderId::DeepSeek
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        let Some(value) = host.deepseek_get()? else {
            return Ok(None);
        };
        let blob = value.into_bytes();
        if api_key(&blob).is_none() {
            return Ok(None);
        }
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        if api_key(blob).is_none() {
            return Err(Error::InvalidData("not a DeepSeek key".into()));
        }
        let value = std::str::from_utf8(blob)
            .map_err(|_| Error::InvalidData("not a DeepSeek key".into()))?;
        host.deepseek_set(value)
    }

    fn details(&self, blob: &[u8]) -> Details {
        let Some(key) = api_key(blob) else {
            return Details::default();
        };
        let digest = Sha256::digest(key.as_bytes());
        let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        Details {
            label: Some(masked(&key)),
            identity: Some(format!("{hex}:")),
            plan: None,
        }
    }

    /// OpenCode signs in by itself; Piaz Switch only parks what it left.
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

    const KEY_A: &str = r#"{"type":3,"key":"sk-aaaaaaaa1111"}"#;
    const KEY_B: &str = r#"{"type":3,"key":"sk-bbbbbbbb2222"}"#;

    #[test]
    fn a_key_is_shown_by_its_tail_and_known_by_its_hash() {
        let a = DeepSeek.details(KEY_A.as_bytes());
        let b = DeepSeek.details(KEY_B.as_bytes());
        assert_eq!(a.label.as_deref(), Some("sk-…1111"));
        assert_ne!(a.identity, b.identity);
        assert_eq!(a.identity, DeepSeek.details(KEY_A.as_bytes()).identity);
        assert!(!format!("{a:?}").contains("aaaaaaaa"));
    }

    #[test]
    fn write_active_puts_the_key_where_opencode_reads_it() {
        let host = FakeHost::new();
        host.put_deepseek(KEY_A);
        DeepSeek.write_active(&host, KEY_B.as_bytes()).unwrap();
        assert_eq!(host.deepseek().as_deref(), Some(KEY_B));
        let live = DeepSeek.read_active(&host).unwrap().unwrap();
        assert_eq!(live.blob, KEY_B.as_bytes());
    }

    #[test]
    fn something_that_is_not_a_key_is_refused_and_not_read() {
        let host = FakeHost::new();
        assert!(DeepSeek.write_active(&host, b"{}").is_err());
        host.put_deepseek(r#"{"type":3}"#);
        assert!(DeepSeek.read_active(&host).unwrap().is_none());
    }
}
