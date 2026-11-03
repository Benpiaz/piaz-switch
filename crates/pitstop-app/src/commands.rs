//! What the window can ask of the app. Every answer is names, emails, plans,
//! usage or an error: never a token. Anything that touches files, the
//! keychain, the network or a lock runs off the UI thread.

use pitstop_core::activity::{self, verb, By, Entry};
use pitstop_core::doctor::{self, Check, Level};
use pitstop_core::engine::{RemoveOutcome, SwitchOutcome};
use pitstop_core::live::Live;
use pitstop_core::{editor, signin, ProviderId};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::app::{self, AppState};
use crate::model::{Snapshot, UiError};
use crate::settings;

type Answer<T> = Result<T, UiError>;

fn provider(id: &str) -> Answer<ProviderId> {
    ProviderId::parse(id).map_err(|e| UiError::from(&e))
}

/// Run blocking work (files, keychain, network, locks) off the async runtime.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("a blocking task panicked")
}

#[tauri::command]
pub async fn get_snapshot(app: AppHandle) -> Option<Snapshot> {
    blocking(move || app::snapshot(&app)).await
}

/// Read the accounts again; with `usage`, ask for usage too.
#[tauri::command]
pub async fn refresh(app: AppHandle, usage: bool) {
    blocking(move || {
        app::refresh(&app);
        // The window regains focus after the user installs a tool, for one.
        if app::look_for_tools(&app, false) {
            app::refresh(&app);
        }
        if usage {
            app::read_usage(&app);
        }
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Switched {
    pub switched: bool,
    /// Nothing happened: the tool holds a login Pitstop does not manage, and
    /// switching would replace it. Ask, then call again with
    /// `replace_unmanaged`.
    pub needs_confirmation: bool,
    pub provider: &'static str,
    pub name: String,
    /// The tool was signed in to someone Pitstop does not manage: that login
    /// was replaced, not parked.
    pub replaced: bool,
    pub replaced_label: Option<String>,
    /// Nothing happened: the tool is an editor that is open, and it reads a
    /// new login only when it starts. Ask, then call again with
    /// `restart_editor`.
    pub needs_restart: bool,
    /// The editor was closed for the switch and opened again.
    pub restarted: bool,
}

/// How long an editor is given to quit when asked.
const EDITOR_QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(25);

/// Switch `pid` to `name`. Unless `replace_unmanaged`, a login no enrolled
/// account owns stops the switch, decided on the same read of the login the
/// switch would park. An editor that is open is closed for the switch and
/// opened again, but only with `restart_editor`; otherwise the answer asks.
pub fn switch(
    app: &AppHandle,
    pid: ProviderId,
    name: &str,
    replace_unmanaged: bool,
    restart_editor: bool,
) -> Answer<Switched> {
    if app::warming(app, pid, name) {
        return Err(UiError {
            code: "warmingUp",
            message: format!("{name} is warming up; switch when it ends"),
            name: Some(name.to_owned()),
        });
    }
    let unchanged = |needs_confirmation, replaced_label| Switched {
        switched: false,
        needs_confirmation,
        provider: pid.as_str(),
        name: name.to_owned(),
        replaced: false,
        replaced_label,
        needs_restart: false,
        restarted: false,
    };
    let state = app.state::<AppState>();
    let mut restarted = false;
    if let Some(editor) = editor::app(pid) {
        // No editor installed, nothing reads the login: say so before asking
        // to close anything (the Antigravity 2 app is not Antigravity's
        // editor, and keeps its login elsewhere).
        let Some(build) = editor.find(&app::real_home()) else {
            return Err(UiError {
                code: "editorMissing",
                message: format!(
                    "Piaz Switch didn't find {} on this computer, so it can't switch it.",
                    editor.name
                ),
                name: Some(pid.display_name().into()),
            });
        };
        // What can be answered without closing it is answered first.
        match app::engine().live(pid) {
            Ok(Live::Account(live)) if live == name => return Ok(unchanged(false, None)),
            Ok(Live::Unmanaged { label, .. }) if !replace_unmanaged => {
                return Ok(unchanged(true, label))
            }
            _ => {}
        }
        if crate::signin::running_for(app, pid) {
            return Err(UiError::new(
                "signInRunning",
                "Another sign-in is already running.",
            ));
        }
        if editor.running() {
            // Another app goes by its program name: Piaz Switch won't close
            // it, so it doesn't offer to.
            if !build.closable() {
                let name = pid.display_name();
                return Err(UiError {
                    code: "editorCloseYourself",
                    message: format!(
                        "{name} is open, and another app on this computer goes by the same program name, so Piaz Switch won't close it for you. Close {name}, then switch again."
                    ),
                    name: Some(name.into()),
                });
            }
            if !restart_editor {
                return Ok(Switched {
                    needs_restart: true,
                    ..unchanged(false, None)
                });
            }
            // Closed without holding the lock: it can take a while. Should it
            // open again meanwhile, the switch below refuses to write under it.
            editor
                .quit(EDITOR_QUIT_WAIT)
                .map_err(|e| UiError::from(&e))?;
            restarted = true;
        }
    }
    let outcome = {
        let _op = state.ops.lock();
        let engine = app::engine();
        let outcome = if replace_unmanaged {
            engine.switch(name, Some(pid))
        } else {
            engine.switch_keeping_unmanaged(name, Some(pid))
        };
        // Back as it was, switched or not.
        if restarted {
            if let Some(editor) = editor::app(pid) {
                let _ = editor.open(&app::real_home());
            }
        }
        outcome
    };
    app::refresh(app);
    let outcome = outcome.map_err(|e| UiError::from(&e))?;
    Ok(match outcome {
        SwitchOutcome::AlreadyActive => unchanged(false, None),
        SwitchOutcome::WouldReplace(login) => unchanged(true, login.label),
        SwitchOutcome::Switched {
            provider,
            name,
            replaced,
        } => {
            app::read_usage(app);
            Switched {
                switched: true,
                needs_confirmation: false,
                provider: provider.as_str(),
                name,
                replaced: replaced.is_some(),
                replaced_label: replaced.and_then(|r| r.label),
                needs_restart: false,
                restarted,
            }
        }
    })
}

/// [`switch`] asked for by the user, in the window or the tray menu: a
/// switch made, or one that failed, goes in the log, and the account left
/// isn't switched back to before the threshold this week.
pub fn switch_by_hand(
    app: &AppHandle,
    pid: ProviderId,
    name: &str,
    replace_unmanaged: bool,
    restart_editor: bool,
) -> Answer<Switched> {
    let from = app::in_use_name(pid);
    let switched = switch(app, pid, name, replace_unmanaged, restart_editor);
    match &switched {
        Ok(done) if done.switched => app::switched_by_hand(app, pid, from.as_deref(), &done.name),
        Err(error) => app::log(
            &app::host(),
            Entry::new(app::now(), verb::USE, By::App)
                .provider(pid)
                .account(name)
                .from(from)
                .result(error.code),
        ),
        Ok(_) => {}
    }
    switched
}

#[tauri::command]
pub async fn use_account(
    app: AppHandle,
    provider: String,
    name: String,
    replace_unmanaged: Option<bool>,
    restart_editor: Option<bool>,
) -> Answer<Switched> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        switch_by_hand(
            &app,
            pid,
            &name,
            replace_unmanaged.unwrap_or(false),
            restart_editor.unwrap_or(false),
        )
    })
    .await
}

/// Enroll the login the tool is signed in to, under `name`.
#[tauri::command]
pub async fn name_account(app: AppHandle, provider: String, name: String) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let enrolled = {
            let _op = state.ops.lock();
            app::engine().enroll(pid, &name)
        };
        app::log(
            &app::host(),
            Entry::new(app::now(), verb::ENROLL, By::App)
                .provider(pid)
                .account(&name)
                .outcome(&enrolled),
        );
        app::refresh(&app);
        enrolled.map_err(|e| UiError::from(&e))?;
        app::read_usage_now(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn rename_account(
    app: AppHandle,
    provider: String,
    name: String,
    new_name: String,
) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let renamed = {
            let _op = state.ops.lock();
            app::engine().rename(&name, Some(pid), &new_name)
        };
        app::log(
            &app::host(),
            Entry::new(app::now(), verb::RENAME, By::App)
                .provider(pid)
                .account(&new_name)
                .from(Some(&name))
                .outcome(&renamed),
        );
        if let Ok(account) = &renamed {
            // The usage read under the old name belongs to the new one, and so
            // do its warm-up setting and record.
            app::rename_usage(&app, pid, &name, &account.name);
            if pid == ProviderId::ClaudeCode {
                crate::warm::follow_rename(&app::host(), &name, &account.name);
            }
            crate::webwin::follow_rename(&app, pid, &name, &account.name);
        }
        app::refresh(&app);
        renamed.map(|_| ()).map_err(|e| UiError::from(&e))
    })
    .await
}

/// Forget an account; `true` when it was the one in use, whose login the
/// tool keeps.
#[tauri::command]
pub async fn forget_account(app: AppHandle, provider: String, name: String) -> Answer<bool> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let removed = {
            let _op = state.ops.lock();
            app::engine().remove(&name, Some(pid))
        };
        app::log(
            &app::host(),
            Entry::new(app::now(), verb::FORGET, By::App)
                .provider(pid)
                .account(&name)
                .outcome(&removed),
        );
        if removed.is_ok() && pid == ProviderId::ClaudeCode {
            crate::warm::follow_forget(&app::host(), &name);
        }
        if removed.is_ok() {
            crate::webwin::follow_forget(&app, pid, &name);
        }
        app::refresh(&app);
        removed
            .map(|outcome| outcome == RemoveOutcome::RemovedActive)
            .map_err(|e| UiError::from(&e))
    })
    .await
}

#[tauri::command]
pub async fn start_sign_in(
    app: AppHandle,
    provider: String,
    name: String,
    again: bool,
) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || crate::signin::start(&app, pid, name, again)).await
}

#[tauri::command]
pub async fn send_sign_in_code(app: AppHandle, code: String) -> Answer<()> {
    blocking(move || {
        let state = app.state::<AppState>();
        let job = state.sign_in.lock().map_err(|_| no_sign_in())?;
        job.as_ref().ok_or_else(no_sign_in)?.send_code(&code)
    })
    .await
}

#[tauri::command]
pub async fn cancel_sign_in(app: AppHandle) {
    blocking(move || crate::signin::cancel(&app)).await
}

/// Open the sign-in page the tool printed. The window never hands over an
/// address of its own: only the one read from the tool is opened.
#[tauri::command]
pub async fn open_sign_in_page(app: AppHandle) -> Answer<()> {
    blocking(move || {
        let url = {
            let state = app.state::<AppState>();
            let job = state.sign_in.lock().map_err(|_| no_sign_in())?;
            job.as_ref().and_then(|j| j.url())
        }
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(no_sign_in)?;
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| UiError::new("open", e.to_string()))
    })
    .await
}

fn no_sign_in() -> UiError {
    UiError::new("signIn", "No sign-in is running.")
}

/// `en`, `vi`, or none to follow the system.
#[tauri::command]
pub async fn set_language(app: AppHandle, language: Option<String>) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.language = language.filter(|l| l == "en" || l == "vi");
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        Ok(())
    })
    .await
}

/// `light`, `dark`, or none to follow the system.
#[tauri::command]
pub async fn set_theme(app: AppHandle, theme: Option<String>) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.theme = theme.filter(|t| t == "light" || t == "dark");
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::apply_window_theme(&app, saved.theme.as_deref());
        app::refresh(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_autostart(app: AppHandle, on: bool) -> Answer<()> {
    blocking(move || {
        let autolaunch = app.autolaunch();
        let changed = if on {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        app::refresh(&app);
        changed.map_err(|e| UiError::new("autostart", e.to_string()))
    })
    .await
}

#[tauri::command]
pub async fn dismiss_tip(app: AppHandle, provider: String) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        if !saved.dismissed_tips.contains(&provider) {
            saved.dismissed_tips.push(provider);
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn quit(app: AppHandle) {
    blocking(move || app::quit(&app)).await
}

/// Automatic switch, run-out notices and the warm-up span; what is left out
/// stays as it is.
#[tauri::command]
pub async fn set_automation(
    app: AppHandle,
    auto_switch: Option<bool>,
    auto_switch_at: Option<u8>,
    use_first: Option<crate::duty::UseFirst>,
    notify_run_out: Option<bool>,
    warm_from: Option<String>,
    warm_to: Option<String>,
) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        if let Some(on) = auto_switch {
            saved.auto_switch = on;
        }
        if let Some(at) = auto_switch_at {
            saved.auto_switch_at = at.clamp(50, 100);
        }
        if let Some(order) = use_first {
            saved.use_first = order;
        }
        if let Some(on) = notify_run_out {
            saved.notify_run_out = on;
        }
        let clock = |c: &String| crate::duty::is_clock(c);
        if let Some(from) = warm_from.filter(clock) {
            saved.warm_from = from;
        }
        if let Some(to) = warm_to.filter(clock) {
            saved.warm_to = to;
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        app::run_duties(&app);
        Ok(())
    })
    .await
}

/// Turn warm-ups on or off for one Claude Code account.
#[tauri::command]
pub async fn set_warm_up(app: AppHandle, name: String, on: bool) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.warm_up.retain(|n| n != &name);
        if on {
            saved.warm_up.push(name);
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        app::run_duties(&app);
        Ok(())
    })
    .await
}

/// The last changes made to the accounts, newest first, from the app and
/// the command line.
#[tauri::command]
pub async fn get_activity() -> Answer<Vec<Entry>> {
    blocking(move || activity::recent(&app::host(), activity::KEEP).map_err(|e| UiError::from(&e)))
        .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub checks: Vec<Check>,
    pub warnings: usize,
    pub failures: usize,
    pub checked_at: i64,
    /// For the copied report: the app's version and the system it runs on.
    pub version: &'static str,
    pub system: String,
}

/// What `piaz-switch doctor` checks, and what the last usage read says of
/// each account that needs something done.
#[tauri::command]
pub async fn run_diagnostics(app: AppHandle) -> Diagnostics {
    blocking(move || {
        // Looked for first: on macOS a look starts a login shell, and nothing
        // waits on it.
        let home = app::real_home();
        let programs: Vec<(ProviderId, Option<std::path::PathBuf>)> = ProviderId::ALL
            .iter()
            .map(|&p| (p, signin::find_program(p, &home)))
            .collect();
        let found = |p: ProviderId| {
            programs
                .iter()
                .find(|(q, _)| *q == p)
                .and_then(|(_, path)| path.clone())
        };
        let mut checks = {
            // A check reads the login in use and the parked ones: not while
            // a switch or a renewal changes them.
            let state = app.state::<AppState>();
            let _op = state.ops.lock();
            doctor::run(&app::engine(), &found)
        };
        if let Some(snapshot) = app::snapshot(&app) {
            usage_checks(&snapshot, &mut checks);
        }
        let (warnings, failures) = doctor::issues(&checks);
        Diagnostics {
            checks,
            warnings,
            failures,
            checked_at: app::now(),
            version: env!("CARGO_PKG_VERSION"),
            system: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        }
    })
    .await
}

/// One check per account whose last usage read needs something done, put
/// after its tool's other checks.
fn usage_checks(snapshot: &Snapshot, checks: &mut Vec<Check>) {
    use crate::model::UsageView;
    for tool in &snapshot.tools {
        for account in &tool.accounts {
            let (level, finding) = match &account.usage {
                Some(UsageView::Unauthorized) => (Level::Fail, "refused"),
                Some(UsageView::Expired) if !account.in_use => (Level::Warn, "expired"),
                Some(UsageView::Failed {
                    rate_limited: true, ..
                }) => (Level::Warn, "rateLimited"),
                Some(UsageView::Failed { offline: true, .. }) => (Level::Warn, "offline"),
                Some(UsageView::Failed { .. }) => (Level::Warn, "unread"),
                _ => continue,
            };
            let check = Check {
                group: tool.id.into(),
                id: "usage",
                level,
                subject: Some(account.name.clone()),
                finding: finding.into(),
            };
            let after = checks
                .iter()
                .rposition(|c| c.group == tool.id)
                .map_or(checks.len(), |i| i + 1);
            checks.insert(after, check);
        }
    }
}
