//! What the app does for programs that run apart from it: the usage it keeps
//! for Claude Code's status line, the `statusline` and `renew` subcommands
//! its own program answers before any window or tray starts, and the
//! Settings controls that hook them up (Claude Code's `statusLine`, the
//! daily renewal job).
//!
//! The app's program answers them, not only the `piaz-switch` command line,
//! because the installers ship the app alone: this program is the one every
//! user has, and on Windows it opens no console window when the scheduler
//! runs it.

use std::path::PathBuf;
use std::sync::Mutex;

use pitstop_core::activity::By;
use pitstop_core::claude_settings::{self, StatusLine};
use pitstop_core::engine::Renewal;
use pitstop_core::presence::{self, Claim, Presence};
use pitstop_core::schedule::{self, RenewRun};
use pitstop_core::usage::Http;
use pitstop_core::usage_cache::{self, status, AccountCache, Cache, ToolCache, WindowCache};
use pitstop_core::{statusline, Host};
use serde::Serialize;

use crate::app;
use crate::model::{Snapshot, ToolView, UiError, UsageView};

// ---------- subcommands, before anything else starts ----------

/// Answer `statusline` or `renew` and say with what code to exit; none for
/// any other start, which goes on to the app.
pub fn headless(args: &[String]) -> Option<i32> {
    use std::io::Write;
    let line = match args.first().map(String::as_str) {
        Some(claude_settings::SUBCOMMAND) => statusline::run(&app::host()),
        Some(schedule::SUBCOMMAND) => renew_report(),
        _ => return None,
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    Some(0)
}

/// Renew the parked logins that are due, unless the app is open, and say
/// what came of it.
fn renew_report() -> String {
    match schedule::renew_now(
        &app::engine(),
        &Http::new(),
        By::Cli,
        app::now(),
        &presence::app_process_running,
    ) {
        Ok(RenewRun::AppOpen) => "Piaz Switch is open and renews parked logins itself.".into(),
        Ok(RenewRun::Unsure) => {
            "Couldn't tell whether Piaz Switch is open, so nothing was renewed.".into()
        }
        Ok(RenewRun::Ran(renewed)) if renewed.is_empty() => "No parked login was due.".into(),
        Ok(RenewRun::Ran(renewed)) => renewed
            .iter()
            .map(|(name, renewal)| match renewal {
                Renewal::Renewed => format!("{name}: renewed"),
                Renewal::Refused => {
                    format!("{name}: refused; sign in to it again in Piaz Switch")
                }
                Renewal::Failed(why) => format!("{name}: not renewed ({why})"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("Couldn't renew parked logins: {e}"),
    }
}

// ---------- the app's presence ----------

static PRESENCE: Mutex<Option<Presence>> = Mutex::new(None);

/// Hold the lock that tells the daily renewal the app is open. False when a
/// renewal holds it right now: the app then leaves renewing to it this time.
pub fn hold_presence() -> bool {
    let mut held = PRESENCE.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_some() {
        return true;
    }
    match presence::claim(&app::host().home()) {
        Claim::Held(lock) => {
            *held = Some(lock);
            true
        }
        Claim::Busy => false,
        // No lock to be had: renewing as before is safer than never.
        Claim::Unknown => true,
    }
}

// ---------- the usage the status line reads ----------

/// A snapshot's tools, read at `read_at`, as the cache keeps them.
pub fn cache_of(read_at: Option<i64>, tools: &[ToolView]) -> Cache {
    Cache {
        read_at,
        tools: tools
            .iter()
            .filter(|t| !t.accounts.is_empty())
            .map(|t| ToolCache {
                provider: t.id.into(),
                accounts: t
                    .accounts
                    .iter()
                    .map(|a| {
                        let (state, windows) = match &a.usage {
                            Some(UsageView::Ok { windows }) => (
                                status::OK,
                                windows
                                    .iter()
                                    .filter(|w| !w.quiet)
                                    .map(|w| WindowCache {
                                        label: w.label.clone(),
                                        percent: w.percent,
                                        resets_at: w.resets_at,
                                        length_secs: w.length_secs,
                                        idle: w.idle,
                                    })
                                    .collect(),
                            ),
                            Some(UsageView::Expired) => (status::EXPIRED, Vec::new()),
                            Some(UsageView::Unauthorized) => (status::REFUSED, Vec::new()),
                            Some(UsageView::Failed { .. }) => (status::FAILED, Vec::new()),
                            None => (status::UNREAD, Vec::new()),
                        };
                        AccountCache {
                            name: a.name.clone(),
                            in_use: a.in_use,
                            plan: a.plan.clone(),
                            status: state.into(),
                            windows,
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// What was last written, so an unchanged snapshot writes nothing.
static WRITTEN: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Keep `snapshot`'s usage for the status line, when it changed.
pub fn keep_usage(snapshot: &Snapshot) {
    let cache = cache_of(snapshot.updated_at, &snapshot.tools);
    let bytes = usage_cache::to_bytes(&cache);
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    if *written == bytes {
        return;
    }
    if usage_cache::save(&app::host(), &cache).is_ok() {
        *written = bytes;
    }
}

// ---------- Settings: Command line ----------

/// This program, as Claude Code and the scheduler run it.
fn program() -> Result<PathBuf, UiError> {
    std::env::current_exe().map_err(|e| UiError::new("io", e.to_string()))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLine {
    /// Claude Code's status line: `absent`, `ours`, `stale` (ours, running
    /// another copy of the app), `other` or `unreadable`.
    pub status_line: &'static str,
    /// The command that shows the line, to add or to copy.
    pub command: String,
    /// Whether this system can run the daily renewal.
    pub keep_fresh_supported: bool,
    pub keep_fresh: bool,
}

fn command_line() -> Result<CommandLine, UiError> {
    let command = claude_settings::command_for(&program()?);
    let status_line = match claude_settings::read(&app::host()) {
        StatusLine::Absent => "absent",
        StatusLine::Ours(line) if line == command => "ours",
        StatusLine::Ours(_) => "stale",
        StatusLine::Other(_) => "other",
        StatusLine::Unreadable => "unreadable",
    };
    let supported = schedule::supported();
    Ok(CommandLine {
        status_line,
        command,
        keep_fresh_supported: supported,
        keep_fresh: supported && schedule::installed(&app::real_home()).unwrap_or(false),
    })
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("a blocking task panicked")
}

#[tauri::command]
pub async fn command_line_status() -> Result<CommandLine, UiError> {
    blocking(command_line).await
}

/// Add Piaz Switch's status line to Claude Code, or take it away; someone
/// else's line is never replaced.
#[tauri::command]
pub async fn set_status_line(on: bool) -> Result<CommandLine, UiError> {
    blocking(move || {
        let host = app::host();
        let changed = if on {
            claude_settings::add(&host, &claude_settings::command_for(&program()?))
        } else {
            claude_settings::remove(&host)
        };
        changed.map_err(|e| UiError::from(&e))?;
        command_line()
    })
    .await
}

/// Have the system renew parked logins once a day, or stop it.
#[tauri::command]
pub async fn set_keep_fresh(on: bool) -> Result<CommandLine, UiError> {
    blocking(move || {
        let home = app::real_home();
        let changed = if on {
            schedule::install(&home, &program()?)
        } else {
            schedule::uninstall(&home)
        };
        changed.map_err(|e| UiError::from(&e))?;
        command_line()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountView, LiveView, WindowView};

    fn window(label: &str, quiet: bool) -> WindowView {
        WindowView {
            label: label.into(),
            percent: 40.0,
            resets_at: Some(10),
            length_secs: Some(18_000),
            quiet,
            idle: false,
        }
    }

    fn account(name: &str, in_use: bool, usage: Option<UsageView>) -> AccountView {
        AccountView {
            name: name.into(),
            label: Some(format!("{name}@example.com")),
            plan: Some("Max 5x".into()),
            in_use,
            parked_at: None,
            usage,
        }
    }

    #[test]
    fn the_cache_keeps_limits_and_states_and_never_an_email() {
        let tools = vec![
            ToolView {
                id: "claude-code",
                name: "Claude Code",
                installed: Some(true),
                live: LiveView::Account {
                    name: "ben1".into(),
                },
                accounts: vec![
                    account(
                        "ben1",
                        true,
                        Some(UsageView::Ok {
                            windows: vec![window("5h", false), window("week Opus", true)],
                        }),
                    ),
                    account("ben2", false, Some(UsageView::Unauthorized)),
                    account("ben3", false, None),
                ],
            },
            ToolView {
                id: "codex",
                name: "Codex",
                installed: Some(false),
                live: LiveView::LoggedOut,
                accounts: Vec::new(),
            },
        ];
        let cache = cache_of(Some(99), &tools);
        assert_eq!(cache.read_at, Some(99));
        // A tool without accounts is left out.
        assert_eq!(cache.tools.len(), 1);
        let accounts = &cache.tool("claude-code").unwrap().accounts;
        assert_eq!(accounts[0].status, status::OK);
        assert!(accounts[0].in_use);
        // A per-model limit nobody used is left out, as in the window.
        assert_eq!(accounts[0].windows.len(), 1);
        assert_eq!(accounts[1].status, status::REFUSED);
        assert_eq!(accounts[2].status, status::UNREAD);
        let json = String::from_utf8(usage_cache::to_bytes(&cache)).unwrap();
        assert!(!json.contains("example.com"));
    }

    #[test]
    fn only_the_two_subcommands_are_answered_before_the_app() {
        assert_eq!(headless(&[]), None);
        assert_eq!(headless(&["--hidden".into()]), None);
        assert_eq!(headless(&["statuslines".into()]), None);
    }
}
