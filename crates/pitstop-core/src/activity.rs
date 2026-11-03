//! What was done to the accounts, and by whom: one JSON line per change in
//! `~/.pitstop/activity.jsonl`, oldest first, the last [`KEEP`] kept.
//!
//! Only names, tools, verbs and outcome codes go in: never a token, an email
//! or an error's own words, which may quote a path or a server's answer.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::Host;

/// How many changes the log keeps.
pub const KEEP: usize = 500;

/// The outcome of a change that worked.
pub const DONE: &str = "done";

/// What asked for the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum By {
    App,
    Cli,
}

/// One change. `verb` is one of the constants in [`verb`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Epoch seconds.
    pub at: i64,
    pub verb: String,
    /// The tool, as `claude-code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The account switched from, or the old name of one renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// [`DONE`], or the code of what stopped it (`notLoggedIn`…).
    pub result: String,
    pub by: By,
    /// Why an automatic change was made, as a code with figures:
    /// `limit|5h|92`, `resetsFirst|<to's reset>|<from's reset>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The verbs the log records.
pub mod verb {
    /// A switch asked for in the app or on the command line.
    pub const USE: &str = "use";
    /// A switch the app made by itself.
    pub const AUTO_SWITCH: &str = "auto-switch";
    /// The login a tool is signed in to, kept under a name.
    pub const ENROLL: &str = "enroll";
    /// An account added through the tool's own sign-in.
    pub const SIGN_IN: &str = "sign-in";
    pub const SIGN_IN_AGAIN: &str = "sign-in-again";
    pub const RENAME: &str = "rename";
    pub const FORGET: &str = "forget";
    /// A parked login renewed before it was read.
    pub const RENEW: &str = "renew";
    pub const WARM_UP: &str = "warm-up";
}

impl Entry {
    pub fn new(at: i64, verb: &str, by: By) -> Self {
        Entry {
            at,
            verb: verb.into(),
            provider: None,
            account: None,
            from: None,
            result: DONE.into(),
            by,
            detail: None,
        }
    }

    pub fn provider(mut self, provider: crate::ProviderId) -> Self {
        self.provider = Some(provider.as_str().into());
        self
    }

    pub fn account(mut self, name: impl Into<String>) -> Self {
        self.account = Some(name.into());
        self
    }

    pub fn from(mut self, name: Option<impl Into<String>>) -> Self {
        self.from = name.map(Into::into);
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The outcome of `result`: done, or its error's code.
    pub fn outcome<T>(mut self, result: &Result<T>) -> Self {
        self.result = match result {
            Ok(_) => DONE.into(),
            Err(e) => e.code().into(),
        };
        self
    }

    pub fn result(mut self, code: impl Into<String>) -> Self {
        self.result = code.into();
        self
    }

    pub fn worked(&self) -> bool {
        self.result == DONE
    }
}

pub fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("activity.jsonl")
}

/// One writer at a time in this process; another process (the app and the
/// command line) may still race it, and then one change goes unlogged.
static WRITING: Mutex<()> = Mutex::new(());

/// Whether `name` could be an account's name: what was typed for one that
/// isn't (an email, a path) never goes in the log.
fn account_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Add `entry` to the log. The log is a record, not a duty: callers go on
/// whatever this answers. A change naming something that isn't an account
/// name, typed for one that doesn't exist or isn't allowed, is left out.
pub fn record(host: &dyn Host, entry: &Entry) -> Result<()> {
    let named = [&entry.account, &entry.from]
        .into_iter()
        .flatten()
        .all(|n| account_name(n));
    if !named {
        return Ok(());
    }
    let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let path = path(host);
    let old = host.read(&path)?.unwrap_or_default();
    let mut lines: Vec<&[u8]> = old
        .split(|&b| b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .collect();
    let line = serde_json::to_vec(entry).unwrap_or_default();
    lines.push(&line);
    let skip = lines.len().saturating_sub(KEEP);
    let mut bytes = Vec::with_capacity(old.len() + line.len() + 1);
    for l in &lines[skip..] {
        bytes.extend_from_slice(l);
        bytes.push(b'\n');
    }
    host.write_atomic(&path, &bytes)
}

/// The last `limit` changes, newest first. A line that can't be read, from a
/// newer version or cut short, is left out.
pub fn recent(host: &dyn Host, limit: usize) -> Result<Vec<Entry>> {
    let bytes = host.read(&path(host))?.unwrap_or_default();
    Ok(bytes
        .split(|&b| b == b'\n')
        .rev()
        .filter_map(|l| serde_json::from_slice::<Entry>(l).ok())
        .take(limit)
        .collect())
}

/// When a switch was last made in `log`, by hand or by itself, from the app
/// or the command line: the automatic switch holds back for a while after one.
pub fn last_switch_at(log: &[Entry]) -> Option<i64> {
    log.iter()
        .filter(|e| e.worked() && (e.verb == verb::USE || e.verb == verb::AUTO_SWITCH))
        .map(|e| e.at)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use crate::{Error, ProviderId};

    fn switch(at: i64, to: &str, from: &str) -> Entry {
        Entry::new(at, verb::USE, By::App)
            .provider(ProviderId::ClaudeCode)
            .account(to)
            .from(Some(from))
    }

    #[test]
    fn changes_come_back_newest_first() {
        let host = FakeHost::new();
        record(&host, &switch(10, "ben2", "ben1")).unwrap();
        record(&host, &switch(20, "ben1", "ben2")).unwrap();
        let log = recent(&host, 10).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].at, 20);
        assert_eq!(log[0].account.as_deref(), Some("ben1"));
        assert_eq!(log[1].from.as_deref(), Some("ben1"));
        assert_eq!(recent(&host, 1).unwrap().len(), 1);
    }

    #[test]
    fn the_log_keeps_only_the_last_changes() {
        let host = FakeHost::new();
        for at in 0..(KEEP as i64 + 7) {
            record(&host, &switch(at, "a", "b")).unwrap();
        }
        let log = recent(&host, usize::MAX).unwrap();
        assert_eq!(log.len(), KEEP);
        assert_eq!(log[0].at, KEEP as i64 + 6);
        assert_eq!(log[KEEP - 1].at, 7);
    }

    #[test]
    fn a_line_that_cant_be_read_is_skipped_not_fatal() {
        let host = FakeHost::new();
        host.put_file(path(&host), b"{not json\n\n".to_vec());
        record(&host, &switch(5, "a", "b")).unwrap();
        let log = recent(&host, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].at, 5);
    }

    #[test]
    fn a_failure_keeps_its_code_and_never_its_words() {
        let failed: Result<()> = Err(Error::VaultBlobMissing("ben3".into()));
        let entry = Entry::new(1, verb::USE, By::Cli).outcome(&failed);
        assert_eq!(entry.result, "vaultBlobMissing");
        assert!(!entry.worked());
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("parked credential"));
        assert!(json.contains("\"by\":\"cli\""));
        // Nothing left empty is written.
        assert!(!json.contains("provider"));
    }

    #[test]
    fn the_last_switch_is_the_latest_that_worked() {
        let host = FakeHost::new();
        assert_eq!(last_switch_at(&recent(&host, KEEP).unwrap()), None);
        record(&host, &switch(10, "a", "b")).unwrap();
        record(&host, &Entry::new(30, verb::AUTO_SWITCH, By::App)).unwrap();
        record(&host, &switch(40, "a", "b").result("vaultCorrupt")).unwrap();
        record(&host, &Entry::new(50, verb::RENAME, By::Cli)).unwrap();
        assert_eq!(last_switch_at(&recent(&host, KEEP).unwrap()), Some(30));
    }

    #[test]
    fn a_name_typed_for_an_account_that_isnt_one_is_never_logged() {
        let host = FakeHost::new();
        let typed = Entry::new(1, verb::FORGET, By::Cli)
            .account("me@example.com")
            .result("unknownAccount");
        record(&host, &typed).unwrap();
        let renamed = Entry::new(2, verb::RENAME, By::App)
            .account("C:\\Users\\ben")
            .from(Some("ben1"));
        record(&host, &renamed).unwrap();
        assert!(recent(&host, 10).unwrap().is_empty());
        record(&host, &switch(3, "ben-2", "ben_1")).unwrap();
        assert_eq!(recent(&host, 10).unwrap().len(), 1);
    }
}
