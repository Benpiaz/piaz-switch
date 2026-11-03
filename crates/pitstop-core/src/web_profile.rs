//! Where each Claude Code or Codex account's web window keeps its profile on
//! Windows and Linux, `~/.pitstop/web/<tool>/<account>`, and keeping those
//! folders in step with the accounts: moved on a rename, deleted on a
//! forget, from the app or the command line. A folder that couldn't go then
//! (a window's browser still held its files) is an orphan, deleted the next
//! time the app starts. On macOS the profile is a data store the app keeps,
//! named by [`store_key`].

use std::io;
use std::path::{Path, PathBuf};

use crate::state::{AccountMeta, State};
use crate::ProviderId;

/// The tools whose accounts get a web window.
pub const TOOLS: [ProviderId; 2] = [ProviderId::ClaudeCode, ProviderId::Codex];

pub fn root(home: &Path) -> PathBuf {
    home.join(".pitstop").join("web")
}

/// Where an account's web profile is kept.
pub fn dir(home: &Path, provider: ProviderId, name: &str) -> PathBuf {
    root(home).join(provider.as_str()).join(name)
}

/// What names an account's profile on macOS: who the account is, which a
/// rename doesn't change, or its name when that isn't known.
pub fn store_key(account: &AccountMeta) -> String {
    match &account.identity {
        Some(identity) => format!("id|{identity}"),
        None => format!("name|{}", account.name),
    }
}

/// The profile follows an account renamed from `old` to `new`. A folder
/// already under the new name can only be an orphan (names are unique per
/// tool), so it goes first. Nothing to move is no failure.
pub fn rename(home: &Path, provider: ProviderId, old: &str, new: &str) -> io::Result<()> {
    let from = dir(home, provider, old);
    if !from.exists() {
        return Ok(());
    }
    let to = dir(home, provider, new);
    if to.exists() {
        std::fs::remove_dir_all(&to)?;
    }
    std::fs::rename(&from, &to)
}

/// The profile of an account forgotten goes. None there is no failure.
pub fn forget(home: &Path, provider: ProviderId, name: &str) -> io::Result<()> {
    let dir = dir(home, provider, name);
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(dir)
}

/// The profile folders whose account no longer exists.
pub fn orphans(home: &Path, state: &State) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for provider in TOOLS {
        let Ok(entries) = std::fs::read_dir(root(home).join(provider.as_str())) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let known = state
                .accounts
                .iter()
                .any(|a| a.provider == provider && a.name == name);
            if !known {
                found.push(path);
            }
        }
    }
    found
}

/// Delete the orphans that can go now; one still held waits for next time.
/// Says how many went.
pub fn sweep(home: &Path, state: &State) -> usize {
    orphans(home, state)
        .into_iter()
        .filter(|path| std::fs::remove_dir_all(path).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(provider: ProviderId, name: &str, identity: Option<&str>) -> AccountMeta {
        AccountMeta {
            name: name.into(),
            provider,
            label: None,
            parked_at: None,
            identity: identity.map(Into::into),
            plan: None,
        }
    }

    fn profile(home: &Path, provider: ProviderId, name: &str) {
        let dir = dir(home, provider, name);
        std::fs::create_dir_all(dir.join("EBWebView")).unwrap();
        std::fs::write(dir.join("EBWebView").join("Cookies"), b"x").unwrap();
    }

    #[test]
    fn a_profile_follows_a_rename_over_an_orphan_and_goes_on_a_forget() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = ProviderId::ClaudeCode;
        profile(home, claude, "work");
        // Left by an account of that name forgotten while its window held it.
        profile(home, claude, "job");
        std::fs::write(dir(home, claude, "job").join("old"), b"stale").unwrap();
        rename(home, claude, "work", "job").unwrap();
        assert!(!dir(home, claude, "work").exists());
        assert!(dir(home, claude, "job")
            .join("EBWebView")
            .join("Cookies")
            .exists());
        assert!(!dir(home, claude, "job").join("old").exists());
        // Nothing to move, or to delete, is no failure.
        rename(home, claude, "nobody", "else").unwrap();
        forget(home, claude, "job").unwrap();
        assert!(!dir(home, claude, "job").exists());
        forget(home, claude, "job").unwrap();
    }

    #[test]
    fn folders_of_accounts_that_are_gone_are_swept() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        profile(home, ProviderId::ClaudeCode, "work");
        profile(home, ProviderId::ClaudeCode, "gone");
        profile(home, ProviderId::Codex, "work");
        let state = State {
            accounts: vec![
                account(ProviderId::ClaudeCode, "work", None),
                // A Cursor account of that name owns no Codex folder.
                account(ProviderId::Cursor, "work", None),
            ],
            ..State::default()
        };
        let mut orphans = orphans(home, &state);
        orphans.sort();
        let mut expected = vec![
            dir(home, ProviderId::ClaudeCode, "gone"),
            dir(home, ProviderId::Codex, "work"),
        ];
        expected.sort();
        assert_eq!(orphans, expected);
        assert_eq!(sweep(home, &state), 2);
        assert!(dir(home, ProviderId::ClaudeCode, "work").exists());
        assert!(super::orphans(home, &state).is_empty());
        // No web folder at all is nothing to sweep.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(sweep(empty.path(), &state), 0);
    }

    #[test]
    fn a_macos_profile_is_named_by_who_the_account_is_not_its_name() {
        let before = account(ProviderId::ClaudeCode, "work", Some("uuid-a:org-1"));
        let after = account(ProviderId::ClaudeCode, "job", Some("uuid-a:org-1"));
        assert_eq!(store_key(&before), store_key(&after));
        let unknown = account(ProviderId::ClaudeCode, "work", None);
        assert_eq!(store_key(&unknown), "name|work");
        assert_ne!(store_key(&unknown), store_key(&before));
    }
}
