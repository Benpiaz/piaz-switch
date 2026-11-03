//! The usage the app last showed, kept in `~/.pitstop/usage.json` for the
//! programs that run apart from it, such as Claude Code's status line, which
//! must answer at once and may not ask the services itself.
//!
//! Only names, plans, which account is in use and the limits go in: never a
//! token or an email.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::Host;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Cache {
    /// When the usage shown was read, epoch seconds; none before the first read.
    pub read_at: Option<i64>,
    pub tools: Vec<ToolCache>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolCache {
    /// The tool, as `claude-code`.
    pub provider: String,
    pub accounts: Vec<AccountCache>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountCache {
    pub name: String,
    pub in_use: bool,
    pub plan: Option<String>,
    /// How its last read went: one of the constants in [`status`].
    pub status: String,
    /// Its limits, when `status` is [`status::OK`].
    pub windows: Vec<WindowCache>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowCache {
    /// `5h`, `week`, `week Opus`…
    pub label: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
    pub length_secs: Option<i64>,
    /// Nothing used and no period running yet.
    pub idle: bool,
}

/// How an account's last read went.
pub mod status {
    pub const OK: &str = "ok";
    /// Its access token lapsed.
    pub const EXPIRED: &str = "expired";
    /// The service refused its login: it has to sign in again.
    pub const REFUSED: &str = "refused";
    /// The read failed (offline, turned away).
    pub const FAILED: &str = "failed";
    /// Not read yet.
    pub const UNREAD: &str = "unread";
}

impl Cache {
    /// The tool's accounts, `claude-code` and so on.
    pub fn tool(&self, provider: &str) -> Option<&ToolCache> {
        self.tools.iter().find(|t| t.provider == provider)
    }
}

pub fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("usage.json")
}

/// The cache as last written; none when there is none or it can't be read.
pub fn load(host: &dyn Host) -> Option<Cache> {
    let bytes = host.read(&path(host)).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// `cache` as the file holds it.
pub fn to_bytes(cache: &Cache) -> Vec<u8> {
    serde_json::to_vec(cache).unwrap_or_default()
}

pub fn save(host: &dyn Host, cache: &Cache) -> Result<()> {
    host.write_atomic(&path(host), &to_bytes(cache))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    pub(crate) fn sample() -> Cache {
        Cache {
            read_at: Some(1_000),
            tools: vec![ToolCache {
                provider: "claude-code".into(),
                accounts: vec![AccountCache {
                    name: "ben1".into(),
                    in_use: true,
                    plan: Some("Max 20x".into()),
                    status: status::OK.into(),
                    windows: vec![WindowCache {
                        label: "5h".into(),
                        percent: 42.0,
                        resets_at: Some(5_000),
                        length_secs: Some(18_000),
                        idle: false,
                    }],
                }],
            }],
        }
    }

    #[test]
    fn the_cache_comes_back_as_it_was_saved() {
        let host = FakeHost::new();
        assert_eq!(load(&host), None);
        save(&host, &sample()).unwrap();
        assert_eq!(load(&host), Some(sample()));
        assert_eq!(
            load(&host).unwrap().tool("claude-code").unwrap().accounts[0].name,
            "ben1"
        );
    }

    #[test]
    fn a_damaged_cache_reads_as_none_and_a_partial_one_gets_defaults() {
        let host = FakeHost::new();
        host.put_file(path(&host), b"{not json".to_vec());
        assert_eq!(load(&host), None);
        host.put_file(path(&host), br#"{"tools":[{"provider":"codex"}]}"#.to_vec());
        let cache = load(&host).unwrap();
        assert_eq!(cache.read_at, None);
        assert!(cache.tool("codex").unwrap().accounts.is_empty());
    }
}
