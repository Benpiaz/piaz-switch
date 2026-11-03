//! What Piaz Switch relies on, checked one thing at a time: its folder, its
//! state, the vault key, and for each tool its program, the login in use and
//! every parked login. `piaz-switch doctor` and the app's Diagnostics show it.
//!
//! A check names what it found by a code, never in words of its own, and
//! names nobody by email: a report is meant to be pasted into a bug report.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::vault_id;
use crate::live::Live;
use crate::{vault, Engine, Host, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// `piaz-switch`, or the tool's id (`claude-code`…).
    pub group: String,
    /// What was checked: one of the constants in [`check`].
    pub id: &'static str,
    pub level: Level,
    /// The account or the path it is about; a path in the home directory
    /// starts with `~`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// What was found: one of the constants in [`found`], or an error's code.
    pub finding: String,
}

/// What a check looks at.
pub mod check {
    /// Piaz Switch's own folder can be written to.
    pub const FOLDER: &str = "folder";
    /// The list of accounts can be read.
    pub const STATE: &str = "state";
    /// The key the parked logins are sealed with is in the keystore.
    pub const VAULT_KEY: &str = "vaultKey";
    /// The tool's program is on this computer.
    pub const PROGRAM: &str = "program";
    /// Whose login the tool is using.
    pub const LOGIN: &str = "login";
    /// An account's parked login can be opened.
    pub const PARKED: &str = "parked";
}

/// What a check found, besides an error's code.
pub mod found {
    pub const WRITABLE: &str = "writable";
    /// The state was read; `subject` counts the accounts.
    pub const READ: &str = "read";
    pub const PRESENT: &str = "present";
    pub const FOUND: &str = "found";
    pub const NOT_FOUND: &str = "notFound";
    /// Signed in to the account in `subject`.
    pub const SIGNED_IN: &str = "signedIn";
    /// Signed in to an account Piaz Switch doesn't manage: added with the
    /// tool's own sign-in, or a sign-in replaced the one it parked.
    pub const UNMANAGED: &str = "unmanaged";
    pub const LOGGED_OUT: &str = "loggedOut";
    /// The parked login is there and opens.
    pub const KEPT: &str = "kept";
}

const APP: &str = "piaz-switch";

impl Check {
    fn new(group: &str, id: &'static str, level: Level, finding: &str) -> Self {
        Check {
            group: group.into(),
            id,
            level,
            subject: None,
            finding: finding.into(),
        }
    }

    fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }
}

/// `path` with the home directory written `~`, so a report names no user.
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => {
            let rest = rest.to_string_lossy();
            if rest.is_empty() {
                "~".into()
            } else {
                format!("~{}{rest}", std::path::MAIN_SEPARATOR)
            }
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

/// Every check, in the order a report lists them. `find` looks for a tool's
/// program, as adding an account would.
pub fn run<H: Host>(
    engine: &Engine<H>,
    find: &dyn Fn(ProviderId) -> Option<PathBuf>,
) -> Vec<Check> {
    let host = engine.host();
    let home = host.home();
    let mut checks = Vec::new();

    let folder = home.join(".pitstop");
    let probe = folder.join(".doctor-probe");
    let wrote = host
        .write_atomic(&probe, b"ok")
        .and_then(|()| host.remove_file(&probe));
    checks.push(
        match &wrote {
            Ok(()) => Check::new(APP, check::FOLDER, Level::Ok, found::WRITABLE),
            Err(e) => Check::new(APP, check::FOLDER, Level::Fail, e.code()),
        }
        .about(tilde(&folder, &home)),
    );

    let state = match engine.state() {
        Ok(state) => {
            checks.push(
                Check::new(APP, check::STATE, Level::Ok, found::READ)
                    .about(state.accounts.len().to_string()),
            );
            state
        }
        Err(e) => {
            checks.push(Check::new(APP, check::STATE, Level::Fail, e.code()));
            return checks;
        }
    };

    if !state.accounts.is_empty() {
        checks.push(match vault::key_present(host) {
            Ok(true) => Check::new(APP, check::VAULT_KEY, Level::Ok, found::PRESENT),
            Ok(false) => Check::new(APP, check::VAULT_KEY, Level::Fail, "vaultKeyMissing"),
            Err(e) => Check::new(APP, check::VAULT_KEY, Level::Fail, e.code()),
        });
    }

    for pid in ProviderId::ALL {
        let group = pid.as_str();
        let accounts: Vec<_> = state
            .accounts
            .iter()
            .filter(|a| a.provider == pid)
            .collect();
        let program = find(pid);
        // A tool with no accounts and no program is none of this report's
        // business.
        if accounts.is_empty() && program.is_none() {
            continue;
        }
        checks.push(match &program {
            Some(path) => Check::new(group, check::PROGRAM, Level::Ok, found::FOUND)
                .about(tilde(path, &dirs_home(&home))),
            None => Check::new(group, check::PROGRAM, Level::Warn, found::NOT_FOUND),
        });

        let live = engine.live(pid);
        let in_use = match &live {
            Ok(Live::Account(name)) => Some(name.clone()),
            _ => None,
        };
        checks.push(match &live {
            Ok(Live::Account(name)) => {
                Check::new(group, check::LOGIN, Level::Ok, found::SIGNED_IN).about(name.clone())
            }
            // Whoever it is, by email, stays out of the report. Only a tool
            // with accounts here has one to lose to a switch.
            Ok(Live::Unmanaged { .. }) if accounts.is_empty() => {
                Check::new(group, check::LOGIN, Level::Ok, found::UNMANAGED)
            }
            Ok(Live::Unmanaged { .. }) => {
                Check::new(group, check::LOGIN, Level::Warn, found::UNMANAGED)
            }
            Ok(Live::LoggedOut) if accounts.is_empty() => {
                Check::new(group, check::LOGIN, Level::Ok, found::LOGGED_OUT)
            }
            Ok(Live::LoggedOut) => Check::new(group, check::LOGIN, Level::Warn, found::LOGGED_OUT),
            Err(e) => Check::new(group, check::LOGIN, Level::Fail, e.code()),
        });

        for account in accounts {
            // The account in use is read from the tool's own login; its vault
            // copy may be old, and nothing needs it until the next switch.
            if in_use.as_deref() == Some(account.name.as_str()) {
                continue;
            }
            let opened = vault::load(host, &vault_id(pid, &account.name));
            checks.push(
                match &opened {
                    Ok(_) => Check::new(group, check::PARKED, Level::Ok, found::KEPT),
                    Err(e) => Check::new(group, check::PARKED, Level::Fail, e.code()),
                }
                .about(account.name.clone()),
            );
        }
    }
    checks
}

/// Programs live under the real home even when the state is kept in a
/// sandbox (`--home`, `PITSTOP_HOME`): a program's path is written with `~`
/// for the real home.
fn dirs_home(sandbox: &Path) -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| sandbox.to_path_buf())
}

/// How many checks warn, and how many fail.
pub fn issues(checks: &[Check]) -> (usize, usize) {
    let count = |level| checks.iter().filter(|c| c.level == level).count();
    (count(Level::Warn), count(Level::Fail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::claude_code::ClaudeCode;
    use crate::testing::FakeHost;

    /// What `/login` does: a new token and that account's `oauthAccount`.
    fn signed_in(host: &FakeHost, email: &str) {
        host.put_file(
            ClaudeCode::credentials_path(host),
            format!(r#"{{"claudeAiOauth":{{"accessToken":"tok-{email}","refreshToken":"R"}}}}"#),
        );
        host.put_file(
            ClaudeCode::config_path(host),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"uuid-{email}","organizationUuid":"org-1"}}}}"#
            ),
        );
    }

    fn of<'a>(checks: &'a [Check], group: &str, id: &str) -> Vec<&'a Check> {
        checks
            .iter()
            .filter(|c| c.group == group && c.id == id)
            .collect()
    }

    #[test]
    fn a_fresh_computer_has_nothing_to_report_but_the_folder_and_the_state() {
        let engine = Engine::new(FakeHost::new());
        let checks = run(&engine, &|_| None);
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.level == Level::Ok));
        assert_eq!(checks[1].subject.as_deref(), Some("0"));
        assert_eq!(issues(&checks), (0, 0));
    }

    #[test]
    fn every_account_and_the_login_in_use_are_checked() {
        let engine = Engine::new(FakeHost::new());
        signed_in(engine.host(), "one@example.com");
        engine.enroll(ProviderId::ClaudeCode, "ben1").unwrap();
        signed_in(engine.host(), "two@example.com");
        engine.enroll(ProviderId::ClaudeCode, "ben2").unwrap();
        let claude = PathBuf::from("/opt/claude");
        let checks = run(&engine, &|p| {
            (p == ProviderId::ClaudeCode).then(|| claude.clone())
        });

        assert_eq!(of(&checks, APP, check::VAULT_KEY)[0].level, Level::Ok);
        let program = of(&checks, "claude-code", check::PROGRAM);
        assert_eq!(program[0].finding, found::FOUND);
        let login = of(&checks, "claude-code", check::LOGIN);
        assert_eq!(login[0].finding, found::SIGNED_IN);
        assert_eq!(login[0].subject.as_deref(), Some("ben2"));
        // Only the parked one: the login in use is the tool's own.
        let parked = of(&checks, "claude-code", check::PARKED);
        assert_eq!(parked.len(), 1);
        assert_eq!(parked[0].subject.as_deref(), Some("ben1"));
        assert_eq!(parked[0].level, Level::Ok);
        // No Codex: neither accounts nor a program.
        assert!(checks.iter().all(|c| c.group != "codex"));
        assert_eq!(issues(&checks), (0, 0));
    }

    #[test]
    fn a_lost_parked_login_fails_and_a_stranger_signed_in_warns_without_an_email() {
        let engine = Engine::new(FakeHost::new());
        signed_in(engine.host(), "one@example.com");
        engine.enroll(ProviderId::ClaudeCode, "ben1").unwrap();
        signed_in(engine.host(), "two@example.com");
        engine.enroll(ProviderId::ClaudeCode, "ben2").unwrap();
        // ben1's parked login goes, and someone else signs in with /login.
        vault::delete(engine.host(), &vault_id(ProviderId::ClaudeCode, "ben1")).unwrap();
        signed_in(engine.host(), "stranger@example.com");
        let checks = run(&engine, &|_| None);

        assert_eq!(
            of(&checks, "claude-code", check::PROGRAM)[0].level,
            Level::Warn
        );
        let login = of(&checks, "claude-code", check::LOGIN)[0];
        assert_eq!(
            (login.level, login.finding.as_str()),
            (Level::Warn, found::UNMANAGED)
        );
        let parked = of(&checks, "claude-code", check::PARKED);
        assert_eq!(parked.len(), 2);
        let ben1 = parked
            .iter()
            .find(|c| c.subject.as_deref() == Some("ben1"))
            .unwrap();
        assert_eq!(
            (ben1.level, ben1.finding.as_str()),
            (Level::Fail, "vaultBlobMissing")
        );
        assert_eq!(issues(&checks), (2, 1));
        let report = serde_json::to_string(&checks).unwrap();
        assert!(!report.contains("example.com"));
    }

    #[test]
    fn a_tool_with_no_accounts_here_signed_in_to_anyone_is_in_order() {
        let engine = Engine::new(FakeHost::new());
        signed_in(engine.host(), "someone@example.com");
        let claude = PathBuf::from("/opt/claude");
        let checks = run(&engine, &|p| {
            (p == ProviderId::ClaudeCode).then(|| claude.clone())
        });
        let login = of(&checks, "claude-code", check::LOGIN)[0];
        assert_eq!(
            (login.level, login.finding.as_str()),
            (Level::Ok, found::UNMANAGED)
        );
        assert_eq!(issues(&checks), (0, 0));
    }

    #[test]
    fn a_missing_vault_key_is_a_failure() {
        let engine = Engine::new(FakeHost::new());
        signed_in(engine.host(), "one@example.com");
        engine.enroll(ProviderId::ClaudeCode, "ben1").unwrap();
        engine.host().delete_keystore_entry("vault-key");
        let checks = run(&engine, &|_| None);
        let key = of(&checks, APP, check::VAULT_KEY)[0];
        assert_eq!(
            (key.level, key.finding.as_str()),
            (Level::Fail, "vaultKeyMissing")
        );
    }

    #[test]
    fn a_path_in_the_home_directory_starts_with_a_tilde() {
        let home = Path::new("/home/ben");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            tilde(&home.join(".pitstop"), home),
            format!("~{sep}.pitstop")
        );
        assert_eq!(tilde(home, home), "~");
        assert_eq!(tilde(Path::new("/opt/claude"), home), "/opt/claude");
    }
}
