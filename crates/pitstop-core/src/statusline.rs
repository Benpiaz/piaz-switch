//! The line Claude Code shows under each session when its `statusLine` runs
//! `piaz-switch statusline`: every Claude Code account's five-hour and weekly
//! use, the one in use first.
//!
//! It reads only the usage the app last showed (see [`crate::usage_cache`]):
//! Claude Code runs it after every response, far too often to ask Anthropic,
//! and it must answer at once.

use std::time::Duration;

use crate::usage_cache::{self, status, AccountCache, Cache, WindowCache};
use crate::Host;

/// A reading older than this gets a `?` after the account's name.
pub const STALE_AFTER: i64 = 15 * 60;

/// Points either side of an even pace that still count as even, as in the
/// app's meters.
const EVEN: f64 = 3.0;

/// The most of Claude Code's input read, and how long it is waited for.
const STDIN_MOST: u64 = 64 * 1024;
const STDIN_WAIT: Duration = Duration::from_millis(300);

const BOLD: &str = "\x1b[1m";
const NORMAL: &str = "\x1b[22m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const PLAIN: &str = "\x1b[39m";

/// The share of a limit used as it stands at `now`: one past its reset has
/// started afresh.
fn used(w: &WindowCache, now: i64) -> f64 {
    if w.resets_at.is_some_and(|at| at <= now) {
        0.0
    } else {
        w.percent
    }
}

/// Which side of an even pace a limit is on: `Some(true)` ahead of it (used
/// faster), `Some(false)` behind, none when even or unknown.
fn ahead(w: &WindowCache, now: i64) -> Option<bool> {
    let (length, resets) = (w.length_secs?, w.resets_at?);
    let left = resets - now;
    if length <= 0 || left <= 0 || left > length {
        return None;
    }
    let expected = 100.0 * (1.0 - left as f64 / length as f64);
    let diff = w.percent - expected;
    (diff.abs() > EVEN).then_some(diff > 0.0)
}

fn limit(w: &WindowCache, now: i64, marked: bool) -> String {
    let mark = match ahead(w, now).filter(|_| marked) {
        Some(true) => format!("{RED}▲{PLAIN}"),
        Some(false) => format!("{GREEN}▼{PLAIN}"),
        None => String::new(),
    };
    format!("{:.0}%{mark}", used(w, now))
}

fn account(a: &AccountCache, stale: bool, now: i64) -> String {
    let name = if a.in_use {
        format!("{BOLD}{}{NORMAL}", a.name)
    } else {
        a.name.clone()
    };
    if a.status != status::OK {
        return format!("{name}?");
    }
    let unsure = if stale { "?" } else { "" };
    let find = |label: &str| a.windows.iter().find(|w| w.label == label);
    let limits: Vec<String> = [find("5h"), find("week")]
        .into_iter()
        .flatten()
        .map(|w| limit(w, now, a.in_use))
        .collect();
    if limits.is_empty() {
        format!("{name}{unsure}")
    } else {
        format!("{name}{unsure} {}", limits.join("·"))
    }
}

/// The line for `cache` at `now`.
pub fn line(cache: Option<&Cache>, now: i64) -> String {
    let Some(cache) = cache else {
        return "piaz-switch: no reading yet".into();
    };
    let accounts = match cache.tool(crate::ProviderId::ClaudeCode.as_str()) {
        Some(tool) if !tool.accounts.is_empty() => &tool.accounts,
        _ => return "piaz-switch: no Claude Code account".into(),
    };
    let stale = cache.read_at.is_none_or(|at| now - at > STALE_AFTER);
    accounts
        .iter()
        .filter(|a| a.in_use)
        .chain(accounts.iter().filter(|a| !a.in_use))
        .map(|a| account(a, stale, now))
        .collect::<Vec<_>>()
        .join("  ")
}

/// Claude Code writes the session to the command's input. Nothing in it is
/// needed, but it is read so Claude Code isn't left writing to a closed pipe;
/// never more than a little of it, and never for long.
fn drain_stdin() {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return;
    }
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut input = Vec::new();
        let _ = std::io::stdin().take(STDIN_MOST).read_to_end(&mut input);
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(STDIN_WAIT);
}

/// What `statusline` prints.
pub fn run(host: &dyn Host) -> String {
    drain_stdin();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    line(usage_cache::load(host).as_ref(), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_cache::ToolCache;

    const NOW: i64 = 1_800_000_000;
    const H: i64 = 3600;

    fn window(label: &str, percent: f64, resets_in: Option<i64>, length: i64) -> WindowCache {
        WindowCache {
            label: label.into(),
            percent,
            resets_at: resets_in.map(|s| NOW + s),
            length_secs: Some(length),
            idle: false,
        }
    }

    fn account(name: &str, in_use: bool, windows: Vec<WindowCache>) -> AccountCache {
        AccountCache {
            name: name.into(),
            in_use,
            plan: None,
            status: status::OK.into(),
            windows,
        }
    }

    fn cache(read_ago: i64, accounts: Vec<AccountCache>) -> Cache {
        Cache {
            read_at: Some(NOW - read_ago),
            tools: vec![ToolCache {
                provider: "claude-code".into(),
                accounts,
            }],
        }
    }

    /// The line without its colours and weight.
    fn plain(line: &str) -> String {
        [BOLD, NORMAL, RED, GREEN, PLAIN]
            .iter()
            .fold(line.to_owned(), |l, code| l.replace(code, ""))
    }

    #[test]
    fn the_account_in_use_comes_first_in_bold_with_its_pace() {
        // Half the five hours gone: 70% is ahead of pace, 20% of the week
        // with six of seven days left is about even, 5% behind.
        let cache = cache(
            60,
            vec![
                account(
                    "work",
                    false,
                    vec![
                        window("5h", 12.0, Some(H), 5 * H),
                        window("week", 40.0, Some(86_400), 7 * 86_400),
                    ],
                ),
                account(
                    "personal",
                    true,
                    vec![
                        window("5h", 70.0, Some(5 * H / 2), 5 * H),
                        window("week", 15.0, Some(6 * 86_400), 7 * 86_400),
                    ],
                ),
            ],
        );
        let line = line(Some(&cache), NOW);
        assert_eq!(plain(&line), "personal 70%▲·15%  work 12%·40%");
        assert!(line.starts_with(&format!("{BOLD}personal{NORMAL}")));
        assert!(line.contains(&format!("70%{RED}▲{PLAIN}")));
        // The other accounts get no pace marks, ahead of it or not.
        assert!(!plain(&line).contains("40%▲"));
    }

    #[test]
    fn a_limit_used_slower_than_pace_gets_a_green_mark() {
        let cache = cache(
            0,
            vec![account(
                "ben",
                true,
                vec![window("5h", 10.0, Some(H), 5 * H)],
            )],
        );
        let line = line(Some(&cache), NOW);
        assert!(line.contains(&format!("10%{GREEN}▼{PLAIN}")));
    }

    #[test]
    fn an_old_reading_is_marked_and_a_limit_past_its_reset_is_unused() {
        let cache = cache(
            STALE_AFTER + 1,
            vec![
                account("ben1", true, vec![window("5h", 100.0, Some(-60), 5 * H)]),
                account("ben2", false, vec![window("5h", 30.0, None, 5 * H)]),
            ],
        );
        assert_eq!(plain(&line(Some(&cache), NOW)), "ben1? 0%  ben2? 30%");
    }

    #[test]
    fn an_account_without_a_week_shows_its_five_hours_alone() {
        let cache = cache(
            0,
            vec![account(
                "seat",
                false,
                vec![window("5h", 33.0, None, 5 * H)],
            )],
        );
        assert_eq!(plain(&line(Some(&cache), NOW)), "seat 33%");
    }

    #[test]
    fn an_account_with_no_reading_shows_its_name_and_a_question_mark() {
        let mut refused = account("old", false, Vec::new());
        refused.status = status::REFUSED.into();
        let cache = cache(0, vec![refused]);
        assert_eq!(plain(&line(Some(&cache), NOW)), "old?");
    }

    #[test]
    fn with_no_cache_or_no_account_it_says_so() {
        assert_eq!(line(None, NOW), "piaz-switch: no reading yet");
        let empty = Cache {
            read_at: Some(NOW),
            tools: Vec::new(),
        };
        assert_eq!(
            line(Some(&empty), NOW),
            "piaz-switch: no Claude Code account"
        );
    }
}
