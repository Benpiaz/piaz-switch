//! What the window and the tray menu show, built from pitstop-core in one
//! place so the two always agree. Pure: no Tauri, no clock, no network.
//!
//! Nothing in here may carry a token: the snapshot goes to the webview as
//! JSON, and only names, emails, plans, usage and states belong there.

use pitstop_core::engine::AccountUsage;
use pitstop_core::live::Live;
use pitstop_core::usage::UsageReport;
use pitstop_core::{Engine, Error, Host, ProviderId};
use serde::Serialize;

use crate::i18n::{self, Lang};
use crate::settings::Settings;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub revision: u64,
    /// Epoch seconds the snapshot was made at, for "resets in" and pace.
    pub now: i64,
    /// A usage read is under way.
    pub reading: bool,
    /// When the last usage read finished.
    pub updated_at: Option<i64>,
    /// Why the accounts could not be read at all.
    pub error: Option<UiError>,
    pub tools: Vec<ToolView>,
    pub suggestions: Vec<Suggestion>,
    pub language: &'static str,
    /// The language chosen in Settings; none follows the system.
    pub language_choice: Option<String>,
    /// The theme chosen in Settings; none follows the system.
    pub theme_choice: Option<String>,
    pub autostart: bool,
    pub dismissed_tips: Vec<String>,
    pub version: &'static str,
    /// Automatic switch, run-out notifications and warm-ups, for Settings.
    pub automation: Automation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub auto_switch: bool,
    pub auto_switch_at: u8,
    pub notify_run_out: bool,
    pub warm_from: String,
    pub warm_to: String,
    /// One per Claude Code account.
    pub warm_up: Vec<WarmRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmRow {
    pub name: String,
    pub plan: Option<String>,
    pub on: bool,
    /// Where it stands, as last judged; none while warm-up is off for it.
    pub standing: Option<crate::duty::WarmStanding>,
    /// When a warm-up last started its period.
    pub warmed_at: Option<i64>,
}

/// What the last round of duties judged of one account's warm-up.
#[derive(Debug, Clone, PartialEq)]
pub struct WarmState {
    pub name: String,
    pub standing: crate::duty::WarmStanding,
    pub warmed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    /// `claude-code` or `codex`.
    pub id: &'static str,
    pub name: &'static str,
    /// Whether its program was found, once looked for.
    pub installed: Option<bool>,
    pub live: LiveView,
    pub accounts: Vec<AccountView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum LiveView {
    /// The tool is signed in to nobody.
    LoggedOut,
    /// Signed in to this enrolled account.
    Account { name: String },
    /// Signed in to someone Pitstop does not manage.
    Unmanaged {
        label: Option<String>,
        plan: Option<String>,
    },
    /// The login could not be read.
    Unknown { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub name: String,
    pub label: Option<String>,
    pub plan: Option<String>,
    pub in_use: bool,
    pub parked_at: Option<u64>,
    pub usage: Option<UsageView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum UsageView {
    Ok {
        windows: Vec<WindowView>,
    },
    /// The access token has lapsed; switching to the account renews it.
    Expired,
    /// The service refused the login: it has to sign in again.
    Unauthorized,
    Failed {
        message: String,
        /// The service turned the read away for coming too often.
        #[serde(rename = "rateLimited")]
        rate_limited: bool,
        /// No answer came back: offline, or a link too slow to finish the read.
        offline: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowView {
    /// `5h`, `week`, `week Opus`…
    pub label: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
    pub length_secs: Option<i64>,
    /// A per-model limit nobody has used: the window counts it in totals
    /// over accounts, not in an account's own list.
    pub quiet: bool,
}

/// The account in use ran out of a limit and another has room.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub provider: &'static str,
    pub from: String,
    pub to: String,
    pub window: String,
}

/// An error as the UI shows it: a code to translate, and the English
/// message for anything without a translation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiError {
    pub code: &'static str,
    pub message: String,
    /// The account an error is about, for the window's own wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl UiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        UiError {
            code,
            message: message.into(),
            name: None,
        }
    }
}

impl From<&Error> for UiError {
    fn from(e: &Error) -> Self {
        let code = match e {
            Error::NotLoggedIn(_) => "notLoggedIn",
            Error::DuplicateAccount(..) => "duplicateAccount",
            Error::AlreadyEnrolled { .. } => "alreadyEnrolled",
            Error::WrongAccount { .. } => "wrongAccount",
            Error::UnknownAccount(_) => "unknownAccount",
            Error::AmbiguousAccount(_) => "ambiguousAccount",
            Error::InvalidName(_) => "invalidName",
            Error::VaultKeyMissing => "vaultKeyMissing",
            Error::VaultBlobMissing(_) => "vaultBlobMissing",
            Error::VaultCorrupt(_) => "vaultCorrupt",
            Error::ToolNotFound(_) => "toolNotFound",
            Error::SignIn(_) => "signIn",
            Error::EditorStillOpen(_) => "editorStillOpen",
            Error::Io { .. } => "io",
            Error::Keystore(_) => "keystore",
            Error::InvalidData(_) => "invalidData",
        };
        let name = match e {
            Error::AlreadyEnrolled { name, .. }
            | Error::WrongAccount { name, .. }
            | Error::DuplicateAccount(name, _)
            | Error::UnknownAccount(name)
            | Error::InvalidName(name)
            | Error::EditorStillOpen(name) => Some(name.clone()),
            _ => None,
        };
        UiError {
            code,
            message: e.to_string(),
            name,
        }
    }
}

/// Everything besides pitstop-core's own state that a snapshot shows.
pub struct Input<'a> {
    pub revision: u64,
    pub now: i64,
    pub usage: &'a [AccountUsage],
    pub reading: bool,
    pub updated_at: Option<i64>,
    pub installed: &'a [(ProviderId, bool)],
    pub settings: &'a Settings,
    pub language: Lang,
    pub autostart: bool,
    /// Warm-up standings from the last round of duties.
    pub warm: &'a [WarmState],
}

pub fn build<H: Host>(engine: &Engine<H>, input: &Input) -> Snapshot {
    let mut snapshot = Snapshot {
        revision: input.revision,
        now: input.now,
        reading: input.reading,
        updated_at: input.updated_at,
        error: None,
        tools: Vec::new(),
        suggestions: Vec::new(),
        language: input.language.code(),
        language_choice: input.settings.language.clone(),
        theme_choice: input.settings.theme.clone(),
        autostart: input.autostart,
        dismissed_tips: input.settings.dismissed_tips.clone(),
        version: env!("CARGO_PKG_VERSION"),
        automation: Automation {
            auto_switch: input.settings.auto_switch,
            auto_switch_at: input.settings.auto_switch_at,
            notify_run_out: input.settings.notify_run_out,
            warm_from: input.settings.warm_from.clone(),
            warm_to: input.settings.warm_to.clone(),
            warm_up: Vec::new(),
        },
    };
    let state = match engine.state() {
        Ok(state) => state,
        Err(e) => {
            snapshot.error = Some(UiError::from(&e));
            return snapshot;
        }
    };

    snapshot.tools = ProviderId::ALL
        .iter()
        .map(|&pid| {
            let installed = input
                .installed
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|(_, found)| *found);
            // An editor's data outlives the editor: once it is gone, the
            // login it left is nobody's business.
            let live = if pid.is_editor() && installed == Some(false) {
                Ok(Live::LoggedOut)
            } else {
                engine.live(pid)
            };
            // An unreadable login leaves the record as the best guess.
            let in_use = match &live {
                Ok(Live::Account(name)) => Some(name.clone()),
                Ok(_) => None,
                Err(_) => state.active_name(pid).cloned(),
            };
            let live = match live {
                Ok(Live::LoggedOut) => LiveView::LoggedOut,
                Ok(Live::Account(name)) => LiveView::Account { name },
                Ok(Live::Unmanaged { label, plan }) => LiveView::Unmanaged { label, plan },
                Err(e) => LiveView::Unknown {
                    message: e.to_string(),
                },
            };
            let accounts = state
                .accounts
                .iter()
                .filter(|a| a.provider == pid)
                .map(|a| {
                    let report = input
                        .usage
                        .iter()
                        .find(|u| u.provider == pid && u.name == a.name)
                        .map(|u| &u.report);
                    AccountView {
                        name: a.name.clone(),
                        label: a.label.clone(),
                        // What the last read says beats what the parked login said.
                        plan: report
                            .and_then(|r| match r {
                                UsageReport::Ok(u) => u.plan.clone(),
                                _ => None,
                            })
                            .or_else(|| a.plan.clone()),
                        in_use: in_use.as_deref() == Some(a.name.as_str()),
                        parked_at: a.parked_at,
                        usage: report.map(UsageView::from),
                    }
                })
                .collect();
            ToolView {
                id: pid.as_str(),
                name: pid.display_name(),
                installed,
                live,
                accounts,
            }
        })
        .collect();
    // A limit past its reset has started afresh, whatever the last reading
    // said; until the next read shows the new share, it is shown unused.
    for account in snapshot
        .tools
        .iter_mut()
        .flat_map(|t| t.accounts.iter_mut())
    {
        if let Some(UsageView::Ok { windows }) = &mut account.usage {
            for w in windows.iter_mut() {
                w.percent = crate::duty::used_now(w, input.now);
            }
        }
    }
    snapshot.suggestions = suggestions(&snapshot.tools);
    snapshot.automation.warm_up = snapshot
        .tools
        .iter()
        .filter(|t| t.id == ProviderId::ClaudeCode.as_str())
        .flat_map(|t| &t.accounts)
        .map(|a| {
            let on = input.settings.warm_up.contains(&a.name);
            let judged = input.warm.iter().find(|w| w.name == a.name);
            WarmRow {
                name: a.name.clone(),
                plan: a.plan.clone(),
                on,
                standing: judged.filter(|_| on).map(|w| w.standing.clone()),
                warmed_at: judged.and_then(|w| w.warmed_at),
            }
        })
        .collect();
    snapshot
}

impl From<&UsageReport> for UsageView {
    fn from(report: &UsageReport) -> Self {
        match report {
            UsageReport::Ok(usage) => UsageView::Ok {
                windows: usage
                    .windows
                    .iter()
                    .map(|w| WindowView {
                        label: w.label.clone(),
                        percent: w.percent,
                        resets_at: w.resets_at,
                        length_secs: w.length_secs,
                        quiet: w.quiet,
                    })
                    .collect(),
            },
            UsageReport::Expired => UsageView::Expired,
            UsageReport::Unauthorized => UsageView::Unauthorized,
            UsageReport::Failed(message) => UsageView::Failed {
                message: message.clone(),
                rate_limited: message == pitstop_core::usage::RATE_LIMITED,
                offline: message.starts_with(pitstop_core::usage::NETWORK),
            },
        }
    }
}

fn windows_of(account: &AccountView) -> Option<&[WindowView]> {
    match &account.usage {
        Some(UsageView::Ok { windows }) => Some(windows),
        _ => None,
    }
}

/// For each tool whose account in use has used up a limit: the other account
/// with the most of that limit left, among those whose usage is known and
/// that have no limit used up. Accounts of one tool may name their limits
/// differently (a Cursor plan renewing weekly or monthly), so one without
/// that limit isn't taken for one with all of it left.
fn suggestions(tools: &[ToolView]) -> Vec<Suggestion> {
    tools
        .iter()
        .filter_map(|tool| {
            let active = tool.accounts.iter().find(|a| a.in_use)?;
            let full = windows_of(active)?.iter().find(|w| w.percent >= 100.0)?;
            let (to, _) = tool
                .accounts
                .iter()
                .filter(|a| !a.in_use)
                .filter_map(|a| {
                    let windows = windows_of(a)?;
                    if windows.iter().any(|w| w.percent >= 100.0) {
                        return None;
                    }
                    let used = windows
                        .iter()
                        .find(|w| w.label == full.label)
                        .map_or(0.0, |w| w.percent);
                    Some((a, used))
                })
                .min_by(|x, y| x.1.total_cmp(&y.1))?;
            Some(Suggestion {
                provider: tool.id,
                from: active.name.clone(),
                to: to.name.clone(),
                window: full.label.clone(),
            })
        })
        .collect()
}

/// One line of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuLine {
    /// A tool's name over its accounts.
    Heading(String),
    /// An account, checked when in use. `id` says what choosing it does.
    Account {
        id: String,
        title: String,
        checked: bool,
    },
    /// Dimmed text under the line before it.
    Note(String),
    Action {
        id: String,
        title: String,
        enabled: bool,
    },
    Separator,
}

/// The tray menu: what needs attention, the accounts by tool, then the
/// commands. `clock` turns epoch seconds into the local `23:58`.
pub fn menu(snapshot: &Snapshot, lang: Lang, clock: &dyn Fn(i64) -> String) -> Vec<MenuLine> {
    let mut lines = Vec::new();
    for suggestion in &snapshot.suggestions {
        lines.push(MenuLine::Action {
            id: format!("suggest|{}|{}", suggestion.provider, suggestion.to),
            title: lang.switch_to(&suggestion.to),
            enabled: true,
        });
        lines.push(MenuLine::Note(
            lang.ran_out(&suggestion.from, &lang.window(&suggestion.window)),
        ));
    }
    if !snapshot.suggestions.is_empty() {
        lines.push(MenuLine::Separator);
    }

    let shown: Vec<&ToolView> = snapshot
        .tools
        .iter()
        .filter(|t| !t.accounts.is_empty() || matches!(t.live, LiveView::Unmanaged { .. }))
        .collect();
    if shown.is_empty() {
        lines.push(MenuLine::Note(lang.no_accounts().into()));
    }
    let headings = shown.len() > 1;
    for (i, tool) in shown.iter().enumerate() {
        if i > 0 {
            lines.push(MenuLine::Separator);
        }
        if headings {
            lines.push(MenuLine::Heading(tool.name.into()));
        }
        for account in &tool.accounts {
            let title = match &account.plan {
                Some(plan) => format!("{} · {plan}", account.name),
                None => account.name.clone(),
            };
            lines.push(MenuLine::Account {
                id: format!("use|{}|{}", tool.id, account.name),
                title,
                checked: account.in_use,
            });
            if let Some(line) = account_line(account, lang, snapshot.now) {
                lines.push(MenuLine::Note(line));
            }
        }
        if let LiveView::Unmanaged { label, .. } = &tool.live {
            lines.push(MenuLine::Account {
                id: format!("name|{}", tool.id),
                title: label.clone().unwrap_or_else(|| lang.unknown_login().into()),
                checked: true,
            });
            lines.push(MenuLine::Note(lang.not_named_yet().into()));
        }
    }

    lines.push(MenuLine::Separator);
    lines.push(MenuLine::Action {
        id: "add".into(),
        title: lang.add_account().into(),
        enabled: true,
    });
    lines.push(MenuLine::Action {
        id: "refresh".into(),
        title: lang.refresh().into(),
        enabled: !snapshot.reading,
    });
    lines.push(MenuLine::Note(if snapshot.reading {
        lang.reading().into()
    } else {
        match snapshot.updated_at {
            Some(at) => lang.updated_at(&clock(at)),
            None => lang.not_read_yet().into(),
        }
    }));
    lines.push(MenuLine::Separator);
    lines.push(MenuLine::Action {
        id: "open".into(),
        title: lang.open_pitstop().into(),
        enabled: true,
    });
    lines.push(MenuLine::Separator);
    lines.push(MenuLine::Action {
        id: "quit".into(),
        title: lang.quit().into(),
        enabled: true,
    });
    lines
}

/// The line under an account: its limits (a limit for one model is left
/// out), or why there are none to show.
fn account_line(account: &AccountView, lang: Lang, now: i64) -> Option<String> {
    match account.usage.as_ref()? {
        UsageView::Ok { windows } => {
            let parts: Vec<String> = windows
                .iter()
                .filter(|w| !w.label.starts_with("week "))
                .map(|w| {
                    let name = lang.window(&w.label);
                    if w.percent >= 100.0 {
                        let after = w
                            .resets_at
                            .filter(|at| *at > now)
                            .map(|at| i18n::duration(at - now));
                        lang.window_used_up(&name, after.as_deref())
                    } else {
                        lang.window_used(&name, w.percent)
                    }
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        UsageView::Expired => Some(lang.token_expired().into()),
        UsageView::Unauthorized => Some(lang.needs_sign_in().into()),
        UsageView::Failed { .. } => Some(lang.usage_unavailable().into()),
    }
}

/// What the menu bar shows beside the icon (macOS) and the tooltip says
/// (Windows): the account in use and its fullest limit, `work 42%`. A limit
/// for one model counts only when there is no other.
pub fn tray_title(snapshot: &Snapshot) -> Option<String> {
    let account = snapshot
        .tools
        .iter()
        .find_map(|t| t.accounts.iter().find(|a| a.in_use))?;
    let fullest = windows_of(account).and_then(|windows| {
        let windows: Vec<&WindowView> = windows.iter().filter(|w| !w.quiet).collect();
        let general: Vec<f64> = windows
            .iter()
            .filter(|w| !w.label.starts_with("week "))
            .map(|w| w.percent)
            .collect();
        let pool: Vec<f64> = if general.is_empty() {
            windows.iter().map(|w| w.percent).collect()
        } else {
            general
        };
        pool.into_iter().reduce(f64::max)
    });
    Some(match fullest {
        Some(percent) => format!("{} {percent:.0}%", account.name),
        None => account.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitstop_core::provider::claude_code::ClaudeCode;
    use pitstop_core::testing::FakeHost;
    use pitstop_core::usage::{Usage, Window};

    #[test]
    fn a_read_turned_away_for_coming_too_often_says_so_to_the_window() {
        let view = |message: &str| {
            serde_json::to_value(UsageView::from(&UsageReport::Failed(message.into()))).unwrap()
        };
        let turned_away = view(pitstop_core::usage::RATE_LIMITED);
        assert_eq!(turned_away["status"], "failed");
        assert_eq!(turned_away["rateLimited"], true);
        assert_eq!(turned_away["offline"], false);
        let unanswered = view("network: timeout: global");
        assert_eq!(unanswered["rateLimited"], false);
        assert_eq!(unanswered["offline"], true);
    }

    const TOKEN_A: &str = "SECRET-TOKEN-AAA";
    const TOKEN_B: &str = "SECRET-TOKEN-BBB";

    fn sign_in_as(host: &FakeHost, token: &str, email: &str) {
        host.put_file(
            ClaudeCode::credentials_path(host),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"R-{token}","subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}}}"#
            ),
        );
        host.put_file(
            ClaudeCode::config_path(host),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"uuid-{email}","organizationUuid":"org"}}}}"#
            ),
        );
    }

    /// `work` (a@x.com) and `personal` (b@x.com) enrolled, `work` in use.
    fn work_and_personal() -> Engine<FakeHost> {
        let engine = Engine::new(FakeHost::new());
        sign_in_as(engine.host(), TOKEN_A, "a@x.com");
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        engine.switch("work", None).unwrap();
        engine
    }

    const NOW: i64 = 1_800_000_000;

    fn window(label: &str, percent: f64) -> Window {
        Window {
            label: label.into(),
            percent,
            resets_at: Some(NOW + 2 * 3_600 + 9 * 60),
            length_secs: Some(18_000),
            quiet: false,
        }
    }

    fn usage_of(name: &str, report: UsageReport) -> AccountUsage {
        AccountUsage {
            provider: ProviderId::ClaudeCode,
            name: name.into(),
            report,
        }
    }

    fn ok(windows: Vec<Window>) -> UsageReport {
        UsageReport::Ok(Usage {
            windows,
            plan: None,
        })
    }

    fn snapshot_of(engine: &Engine<FakeHost>, usage: &[AccountUsage]) -> Snapshot {
        let settings = Settings::default();
        build(
            engine,
            &Input {
                revision: 1,
                now: NOW,
                usage,
                reading: false,
                updated_at: Some(NOW),
                installed: &[],
                settings: &settings,
                language: Lang::En,
                autostart: false,
                warm: &[],
            },
        )
    }

    fn claude(snapshot: &Snapshot) -> &ToolView {
        snapshot
            .tools
            .iter()
            .find(|t| t.id == "claude-code")
            .unwrap()
    }

    fn in_use(snapshot: &Snapshot) -> Vec<&str> {
        claude(snapshot)
            .accounts
            .iter()
            .filter(|a| a.in_use)
            .map(|a| a.name.as_str())
            .collect()
    }

    #[test]
    fn the_account_in_use_is_the_one_the_tool_is_signed_in_to() {
        let engine = work_and_personal();
        assert_eq!(in_use(&snapshot_of(&engine, &[])), ["work"]);
        // personal signs in with the tool itself; the record still says work
        sign_in_as(engine.host(), "SECRET-TOKEN-B2", "b@x.com");
        let snapshot = snapshot_of(&engine, &[]);
        assert_eq!(in_use(&snapshot), ["personal"]);
        assert_eq!(
            claude(&snapshot).live,
            LiveView::Account {
                name: "personal".into()
            }
        );
    }

    #[test]
    fn a_login_nobody_enrolled_is_shown_as_unmanaged_and_in_use_by_no_account() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), "SECRET-TOKEN-CCC", "c@x.com");
        let snapshot = snapshot_of(&engine, &[]);
        assert!(in_use(&snapshot).is_empty());
        assert_eq!(
            claude(&snapshot).live,
            LiveView::Unmanaged {
                label: Some("c@x.com".into()),
                plan: Some("Max 20x".into())
            }
        );
    }

    #[test]
    fn a_read_that_names_the_plan_shows_it_over_the_recorded_one() {
        let engine = work_and_personal();
        let read = UsageReport::Ok(Usage {
            windows: vec![window("5h", 42.0)],
            plan: Some("Max 5x".into()),
        });
        let snapshot = snapshot_of(&engine, &[usage_of("work", read)]);
        let accounts = &claude(&snapshot).accounts;
        assert_eq!(accounts[0].plan.as_deref(), Some("Max 5x"));
        // a read that doesn't name it leaves the recorded plan
        let snapshot = snapshot_of(&engine, &[usage_of("work", ok(vec![]))]);
        assert_eq!(
            claude(&snapshot).accounts[0].plan.as_deref(),
            Some("Max 20x")
        );
    }

    #[test]
    fn accounts_carry_their_email_plan_and_usage() {
        let engine = work_and_personal();
        let usage = [usage_of("work", ok(vec![window("5h", 42.0)]))];
        let snapshot = snapshot_of(&engine, &usage);
        let work = &claude(&snapshot).accounts[0];
        assert_eq!(work.name, "work");
        assert_eq!(work.label.as_deref(), Some("a@x.com"));
        assert_eq!(work.plan.as_deref(), Some("Max 20x"));
        assert_eq!(
            work.usage,
            Some(UsageView::Ok {
                windows: vec![WindowView {
                    label: "5h".into(),
                    percent: 42.0,
                    resets_at: Some(NOW + 2 * 3_600 + 9 * 60),
                    length_secs: Some(18_000),
                    quiet: false,
                }]
            })
        );
    }

    #[test]
    fn no_token_reaches_the_window() {
        let engine = work_and_personal();
        let usage = [usage_of("work", ok(vec![window("5h", 42.0)]))];
        let json = serde_json::to_string(&snapshot_of(&engine, &usage)).unwrap();
        for secret in [TOKEN_A, TOKEN_B, "R-SECRET"] {
            assert!(!json.contains(secret), "{secret} leaked into {json}");
        }
        assert!(
            json.contains("a@x.com"),
            "the snapshot is not empty: {json}"
        );
    }

    #[test]
    fn running_out_suggests_the_account_with_the_most_room() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), "SECRET-TOKEN-DDD", "d@x.com");
        engine.enroll(ProviderId::ClaudeCode, "spare").unwrap();
        engine.switch("work", None).unwrap();
        let usage = [
            usage_of("work", ok(vec![window("5h", 100.0)])),
            usage_of("personal", ok(vec![window("5h", 60.0)])),
            usage_of("spare", ok(vec![window("5h", 20.0)])),
        ];
        assert_eq!(
            snapshot_of(&engine, &usage).suggestions,
            [Suggestion {
                provider: "claude-code",
                from: "work".into(),
                to: "spare".into(),
                window: "5h".into(),
            }]
        );
    }

    #[test]
    fn an_account_used_up_under_another_limit_name_is_not_suggested() {
        let engine = work_and_personal();
        let week = Window {
            label: "week".into(),
            ..window("5h", 100.0)
        };
        let month = Window {
            label: "month".into(),
            ..window("5h", 100.0)
        };
        let usage = [
            usage_of("work", ok(vec![week])),
            usage_of("personal", ok(vec![month])),
        ];
        assert!(snapshot_of(&engine, &usage).suggestions.is_empty());
    }

    #[test]
    fn nothing_is_suggested_without_an_account_known_to_have_room() {
        let engine = work_and_personal();
        let usage = [
            usage_of("work", ok(vec![window("5h", 100.0)])),
            usage_of("personal", UsageReport::Expired),
        ];
        assert!(snapshot_of(&engine, &usage).suggestions.is_empty());
    }

    fn clock(_: i64) -> String {
        "23:58".into()
    }

    fn notes(lines: &[MenuLine]) -> Vec<&str> {
        lines
            .iter()
            .filter_map(|l| match l {
                MenuLine::Note(n) => Some(n.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_lists_accounts_with_a_check_on_the_one_in_use_and_their_usage() {
        let engine = work_and_personal();
        let usage = [
            usage_of("work", ok(vec![window("5h", 42.0), window("week", 12.0)])),
            usage_of("personal", UsageReport::Unauthorized),
        ];
        let lines = menu(&snapshot_of(&engine, &usage), Lang::En, &clock);
        assert_eq!(
            lines[0],
            MenuLine::Account {
                id: "use|claude-code|work".into(),
                title: "work · Max 20x".into(),
                checked: true,
            }
        );
        assert_eq!(lines[1], MenuLine::Note("5-hour 42%, weekly 12%".into()));
        assert_eq!(
            lines[2],
            MenuLine::Account {
                id: "use|claude-code|personal".into(),
                title: "personal · Max 20x".into(),
                checked: false,
            }
        );
        assert_eq!(lines[3], MenuLine::Note("Needs signing in again".into()));
        assert!(lines.contains(&MenuLine::Action {
            id: "add".into(),
            title: "Add Account…".into(),
            enabled: true
        }));
        assert!(notes(&lines).contains(&"Updated 23:58"));
    }

    /// The tray puts the author's links just above these two lines.
    #[test]
    fn the_menu_ends_with_a_separator_and_quit() {
        let engine = work_and_personal();
        let lines = menu(&snapshot_of(&engine, &[]), Lang::En, &clock);
        let n = lines.len();
        assert_eq!(lines[n - 2], MenuLine::Separator);
        assert!(
            matches!(&lines[n - 1], MenuLine::Action { id, .. } if id == "quit"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_used_up_limit_says_when_it_resets() {
        let engine = work_and_personal();
        let usage = [usage_of("work", ok(vec![window("5h", 100.0)]))];
        let lines = menu(&snapshot_of(&engine, &usage), Lang::En, &clock);
        assert!(
            notes(&lines).contains(&"5-hour used up, resets in 2h 09m"),
            "{lines:?}"
        );
    }

    #[test]
    fn the_menu_offers_the_suggested_switch_first() {
        let engine = work_and_personal();
        let usage = [
            usage_of("work", ok(vec![window("5h", 100.0)])),
            usage_of("personal", ok(vec![window("5h", 10.0)])),
        ];
        let lines = menu(&snapshot_of(&engine, &usage), Lang::En, &clock);
        assert_eq!(
            lines[0],
            MenuLine::Action {
                id: "suggest|claude-code|personal".into(),
                title: "Switch to personal".into(),
                enabled: true
            }
        );
        assert_eq!(
            lines[1],
            MenuLine::Note("work has no 5-hour limit left".into())
        );
    }

    #[test]
    fn an_unmanaged_login_is_offered_for_naming() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), "SECRET-TOKEN-CCC", "c@x.com");
        let lines = menu(&snapshot_of(&engine, &[]), Lang::En, &clock);
        let at = lines
            .iter()
            .position(|l| {
                *l == MenuLine::Account {
                    id: "name|claude-code".into(),
                    title: "c@x.com".into(),
                    checked: true,
                }
            })
            .expect("the unmanaged login is listed");
        assert_eq!(lines[at + 1], MenuLine::Note("Not named yet".into()));
    }

    #[test]
    fn tool_headings_appear_once_two_tools_have_accounts() {
        let engine = work_and_personal();
        let lines = menu(&snapshot_of(&engine, &[]), Lang::En, &clock);
        assert!(!lines.iter().any(|l| matches!(l, MenuLine::Heading(_))));

        use base64::Engine as _;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let id_token = format!("h.{}.s", b64(r#"{"email":"cx@x.com","sub":"u"}"#));
        engine.host().put_file(
            engine.host().home().join(".codex").join("auth.json"),
            format!(r#"{{"tokens":{{"id_token":"{id_token}","access_token":"SECRET-CX"}}}}"#),
        );
        engine.enroll(ProviderId::Codex, "main").unwrap();
        let lines = menu(&snapshot_of(&engine, &[]), Lang::En, &clock);
        let headings: Vec<_> = lines
            .iter()
            .filter_map(|l| match l {
                MenuLine::Heading(h) => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headings, ["Claude Code", "Codex"]);
    }

    #[test]
    fn the_tray_title_is_the_account_in_use_and_its_fullest_limit() {
        let engine = work_and_personal();
        let usage = [usage_of(
            "work",
            ok(vec![window("5h", 42.4), window("week", 61.0)]),
        )];
        assert_eq!(
            tray_title(&snapshot_of(&engine, &usage)).as_deref(),
            Some("work 61%")
        );
        assert_eq!(
            tray_title(&snapshot_of(&engine, &[])).as_deref(),
            Some("work")
        );
    }

    #[test]
    fn a_limit_whose_reset_has_passed_reads_as_unused_until_it_is_read_again() {
        let engine = work_and_personal();
        engine.switch("work", None).unwrap();
        let reset = Window {
            resets_at: Some(NOW - 60),
            ..window("5h", 100.0)
        };
        let usage = [
            usage_of("work", ok(vec![reset])),
            usage_of("personal", ok(vec![window("5h", 20.0)])),
        ];
        let snapshot = snapshot_of(&engine, &usage);
        let work = claude(&snapshot)
            .accounts
            .iter()
            .find(|a| a.name == "work")
            .unwrap();
        match &work.usage {
            Some(UsageView::Ok { windows }) => assert_eq!(windows[0].percent, 0.0),
            other => panic!("{other:?}"),
        }
        assert!(snapshot.suggestions.is_empty());
    }
}
