//! The notices already posted, kept in `~/.pitstop/notified.json` so a
//! restart doesn't post them again. A key holds a tool, account names, a
//! limit and a time: nothing secret. Each is kept until the time it is about
//! has passed, or, without one, until it is forgotten.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pitstop_core::Host;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notified {
    /// Each key, and when it may be forgotten (epoch seconds).
    keys: BTreeMap<String, Option<i64>>,
}

/// How long a key with no time of its own to wait for is kept at most.
const KEEP_AT_MOST: i64 = 8 * 86_400;

fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("notified.json")
}

impl Notified {
    /// What was posted before; a missing or unreadable file is nothing.
    pub fn load(host: &dyn Host) -> Self {
        host.read(&path(host))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Kept for next time; failing only means a notice may come again.
    pub fn save(&self, host: &dyn Host) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = host.write_atomic(&path(host), &bytes);
        }
    }

    /// `key` was posted; it may be forgotten after `until`.
    pub fn insert(&mut self, key: String, until: Option<i64>) {
        self.keys.insert(key, until);
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.keys.remove(key).is_some()
    }

    pub fn keys(&self) -> HashSet<String> {
        self.keys.keys().cloned().collect()
    }

    /// Forget what is over: keys whose time has passed. True when any went.
    pub fn prune(&mut self, now: i64) -> bool {
        let before = self.keys.len();
        self.keys.retain(|_, until| until.is_none_or(|at| at > now));
        self.keys.len() != before
    }

    /// When a key about a limit may be forgotten: once the limit resets, or
    /// after a while when its reset isn't known.
    pub fn until(resets_at: Option<i64>, now: i64) -> Option<i64> {
        Some(resets_at.unwrap_or(now + KEEP_AT_MOST))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitstop_core::testing::FakeHost;

    #[test]
    fn what_was_notified_survives_a_restart_until_its_time_passes() {
        let host = FakeHost::new();
        let mut notified = Notified::load(&host);
        notified.insert("near|codex|main|5h|100".into(), Some(100));
        notified.insert("signin|claude-code|ben3".into(), None);
        notified.save(&host);

        let mut again = Notified::load(&host);
        assert!(again.keys().contains("near|codex|main|5h|100"));
        assert!(again.keys().contains("signin|claude-code|ben3"));
        assert!(again.prune(150));
        assert!(!again.keys().contains("near|codex|main|5h|100"));
        assert!(again.keys().contains("signin|claude-code|ben3"));
        assert!(again.remove("signin|claude-code|ben3"));
        assert!(again.keys().is_empty());
    }

    #[test]
    fn a_missing_or_damaged_file_is_nothing_notified() {
        let host = FakeHost::new();
        assert!(Notified::load(&host).keys().is_empty());
        host.put_file(path(&host), b"{not json".to_vec());
        assert!(Notified::load(&host).keys().is_empty());
    }

    #[test]
    fn a_limit_whose_reset_is_unknown_is_kept_for_a_while() {
        assert_eq!(Notified::until(Some(500), 100), Some(500));
        assert_eq!(Notified::until(None, 100), Some(100 + KEEP_AT_MOST));
    }
}
