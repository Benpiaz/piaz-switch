//! The app's running state: the last usage read, the snapshot built from it,
//! and the one place that rebuilds both the window and the tray menu.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pitstop_core::engine::AccountUsage;
use pitstop_core::live::Live;
use pitstop_core::usage::Http;
use pitstop_core::{signin, Engine, ProviderId, RealHost};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

use crate::duty::{self, WarmStanding};
use crate::i18n::Lang;
use crate::model::{self, Snapshot, WarmState};
use crate::{settings, warm};

/// How often the app looks whether a warm-up, switch or notice is due.
const DUTIES_EVERY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct AppState {
    inner: Mutex<Inner>,
    /// One change at a time: a switch from the menu and one from the window
    /// never interleave.
    pub ops: Mutex<()>,
    pub sign_in: Mutex<Option<crate::signin::SignInJob>>,
    /// A round of duties is under way; another is not started over it.
    duties: AtomicBool,
    automatic: Mutex<Automatic>,
}

/// What the automatic switch and the notices remember while the app runs.
#[derive(Default)]
struct Automatic {
    /// Limits notified, or handled by a switch, until they reset.
    sent: HashSet<String>,
    /// Automatic switches tried per limit until it resets.
    tries: HashMap<String, u32>,
    /// Told once that an unmanaged login stops automatic switching.
    told_unmanaged: bool,
}

#[derive(Default)]
struct Inner {
    revision: u64,
    usage: Vec<AccountUsage>,
    reads: duty::Reads,
    updated_at: Option<i64>,
    installed: Vec<(ProviderId, bool)>,
    snapshot: Option<Snapshot>,
    warm: Vec<WarmState>,
}

/// Where Pitstop keeps its state: the home directory, or `PITSTOP_HOME` to
/// try the app on a sandbox without touching real accounts.
pub fn host() -> RealHost {
    std::env::var_os("PITSTOP_HOME")
        .map(|home| RealHost::with_home(home.into()))
        .unwrap_or_default()
}

/// The real home, where installers put `claude` and `codex`.
pub fn real_home() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default()
}

pub fn engine() -> Engine<RealHost> {
    Engine::new(host())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn snapshot(app: &AppHandle) -> Option<Snapshot> {
    let state = app.state::<AppState>();
    let inner = state.inner.lock().ok()?;
    inner.snapshot.clone()
}

/// Read the accounts again and show them, in the window and the tray menu.
pub fn refresh(app: &AppHandle) {
    let host = host();
    let settings = settings::load(&host);
    let lang = Lang::resolve(settings.language.as_deref());
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    let engine = Engine::new(host);

    // Copy what the snapshot needs, then read the accounts (files, and the
    // keychain on macOS) without holding the lock.
    let state = app.state::<AppState>();
    let (revision, usage, reading, updated_at, installed, warm) = {
        let Ok(mut inner) = state.inner.lock() else {
            return;
        };
        inner.revision += 1;
        (
            inner.revision,
            inner.usage.clone(),
            inner.reads.busy(),
            inner.updated_at,
            inner.installed.clone(),
            inner.warm.clone(),
        )
    };
    let snapshot = model::build(
        &engine,
        &model::Input {
            revision,
            now: now(),
            usage: &usage,
            reading,
            updated_at,
            installed: &installed,
            settings: &settings,
            language: lang,
            autostart,
            warm: &warm,
        },
    );
    {
        let Ok(mut inner) = state.inner.lock() else {
            return;
        };
        // A refresh that started later may have finished first.
        if inner
            .snapshot
            .as_ref()
            .is_some_and(|shown| shown.revision > revision)
        {
            return;
        }
        inner.snapshot = Some(snapshot.clone());
    }
    let _ = app.emit("snapshot", &snapshot);
    crate::tray::update(app, &snapshot, lang);
}

/// Usage read under an account's old name now belongs to its new one.
pub fn rename_usage(app: &AppHandle, provider: ProviderId, old: &str, new: &str) {
    let state = app.state::<AppState>();
    if let Ok(mut inner) = state.inner.lock() {
        for usage in &mut inner.usage {
            if usage.provider == provider && usage.name == old {
                new.clone_into(&mut usage.name);
            }
        }
    };
}

/// Stop a sign-in that is running, then quit.
pub fn quit(app: &AppHandle) {
    crate::signin::cancel(app);
    app.exit(0);
}

/// Ask Anthropic and OpenAI for every account's usage, off the calling
/// thread; a read already under way is not started twice.
pub fn read_usage(app: &AppHandle) {
    read(app, false);
}

/// [`read_usage`] even right after another read: an account just added has
/// no reading at all.
pub fn read_usage_now(app: &AppHandle) {
    read(app, true);
}

/// Ask for a read; [`duty::Reads`] says when it happens.
fn read(app: &AppHandle, now_anyway: bool) {
    let plan = {
        let state = app.state::<AppState>();
        let Ok(mut inner) = state.inner.lock() else {
            return;
        };
        inner.reads.ask(now_anyway, now())
    };
    follow(app, plan);
}

fn follow(app: &AppHandle, plan: duty::ReadPlan) {
    match plan {
        duty::ReadPlan::Nothing => {}
        duty::ReadPlan::Start => start_read(app),
        duty::ReadPlan::WaitUntil(due) => {
            let later = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs((due - now()).max(0) as u64));
                let plan = {
                    let state = later.state::<AppState>();
                    let Ok(mut inner) = state.inner.lock() else {
                        return;
                    };
                    inner.reads.turn_came(due, now())
                };
                follow(&later, plan);
            });
            // Refresh spins while the read waits for its turn.
            refresh(app);
        }
    }
}

/// Read every account's usage, off the calling thread.
fn start_read(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        refresh(&app);
        // However the read ends, the next one may start.
        let read_now = std::panic::catch_unwind(|| engine().usage(&Http::new()));
        let plan = {
            let state = app.state::<AppState>();
            let Ok(mut inner) = state.inner.lock() else {
                return;
            };
            if let Ok(Ok(usage)) = read_now {
                let (usage, updated_at) =
                    duty::merge_read(&inner.usage, inner.updated_at, usage, now());
                inner.usage = usage;
                inner.updated_at = updated_at;
            }
            inner.reads.ended(now())
        };
        refresh(&app);
        follow(&app, plan);
        run_duties(&app);
    });
}

// ---------- duties: automatic switch, run-out notices, warm-ups ----------

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

/// One round: decide from the last snapshot, act, and show what came of it.
/// Never two rounds at once; a warm-up can take a while.
pub fn run_duties(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.duties.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let finished =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| duties(&app))).is_ok();
        end_round(&app, finished);
    });
}

/// Ends a round of duties however it ended, a panic included: the next round
/// may start, and no account stays shown as warming up (which keeps it from
/// being switched to); the window is told at once.
fn end_round(app: &AppHandle, finished: bool) {
    let state = app.state::<AppState>();
    let mut stuck = false;
    if !finished {
        // A panic while the state was held must not stop the app updating.
        state.inner.clear_poison();
    }
    {
        let mut inner = state.inner.lock().unwrap_or_else(|e| e.into_inner());
        for w in &mut inner.warm {
            if w.standing == WarmStanding::Warming {
                w.standing = WarmStanding::Due;
                stuck = true;
            }
        }
    }
    state.duties.store(false, Ordering::SeqCst);
    if stuck || !finished {
        refresh(app);
    }
}

/// Whether `name`'s login is lent to a warm-up right now: a switch to it
/// would put in use a login being renewed.
pub fn warming(app: &AppHandle, provider: ProviderId, name: &str) -> bool {
    provider == ProviderId::ClaudeCode
        && app.state::<AppState>().inner.lock().is_ok_and(|inner| {
            inner
                .warm
                .iter()
                .any(|w| w.name == name && w.standing == WarmStanding::Warming)
        })
}

fn duties(app: &AppHandle) {
    let Some(snapshot) = snapshot(app) else {
        return;
    };
    if snapshot.reading || snapshot.updated_at.is_none() {
        return;
    }
    let host = host();
    let settings = settings::load(&host);
    let lang = Lang::resolve(settings.language.as_deref());
    let claude = snapshot
        .tools
        .iter()
        .find(|t| t.id == ProviderId::ClaudeCode.as_str());

    if settings.auto_switch {
        if let Some(plan) =
            claude.and_then(|tool| duty::switch_target(tool, settings.auto_switch_at, now()))
        {
            auto_switch(app, lang, &plan);
        }
    }
    if settings.notify_run_out {
        let state = app.state::<AppState>();
        let fresh = {
            let Ok(mut auto) = state.automatic.lock() else {
                return;
            };
            let fresh =
                duty::run_out_notices(&snapshot.tools, &snapshot.suggestions, &auto.sent, now());
            auto.sent.extend(fresh.iter().map(|n| n.key.clone()));
            fresh
        };
        for notice in fresh {
            notify(
                app,
                &lang.ran_out(&notice.from, &lang.window(&notice.window)),
                &lang.still_has_room(&notice.to),
            );
        }
    }
    if let Some(tool) = claude {
        warm_ups(app, &host, &settings, tool);
    }
}

fn auto_switch(app: &AppHandle, lang: Lang, plan: &duty::SwitchPlan) {
    let key = plan.key(ProviderId::ClaudeCode.as_str());
    let state = app.state::<AppState>();
    {
        let Ok(mut auto) = state.automatic.lock() else {
            return;
        };
        let tries = auto.tries.entry(key.clone()).or_default();
        if *tries >= duty::SWITCH_TRIES {
            return;
        }
        *tries += 1;
    }
    let tool = ProviderId::ClaudeCode.display_name();
    match crate::commands::switch(app, ProviderId::ClaudeCode, &plan.to, false) {
        Ok(done) if done.switched => {
            if let Ok(mut auto) = state.automatic.lock() {
                auto.sent.insert(key);
                auto.told_unmanaged = false;
            }
            let title = lang.auto_switched(tool, &plan.to);
            let why = lang.auto_switched_why(&plan.from, plan.percent, &lang.window(&plan.limit));
            notify(app, &title, &why);
            let _ = app.emit(
                "auto-switched",
                serde_json::json!({ "provider": done.provider, "name": done.name, "title": title, "why": why }),
            );
        }
        Ok(done) if done.needs_confirmation => {
            let first = state
                .automatic
                .lock()
                .map(|mut auto| !std::mem::replace(&mut auto.told_unmanaged, true))
                .unwrap_or(false);
            if first {
                notify(app, lang.cant_auto_switch(), lang.cant_auto_switch_why());
            }
        }
        _ => {}
    }
}

/// Today in this machine's time zone, and minutes since its midnight.
fn local_day() -> (String, u32) {
    use chrono::Timelike;
    let now = chrono::Local::now();
    (
        now.format("%Y-%m-%d").to_string(),
        now.hour() * 60 + now.minute(),
    )
}

/// Show where one account's warm-up stands as soon as it changes: warming
/// up while it runs (a parked account can't be switched to meanwhile, and the
/// window says why), what came of it right after.
fn show_warm(app: &AppHandle, judged: WarmState) {
    {
        let state = app.state::<AppState>();
        if let Ok(mut inner) = state.inner.lock() {
            match inner.warm.iter_mut().find(|w| w.name == judged.name) {
                Some(w) => *w = judged,
                None => inner.warm.push(judged),
            }
        };
    }
    refresh(app);
}

fn warm_ups(
    app: &AppHandle,
    host: &RealHost,
    settings: &settings::Settings,
    tool: &model::ToolView,
) {
    let (today, minute) = local_day();
    let in_span = duty::in_span(minute, &settings.warm_from, &settings.warm_to);
    let mut ledger = warm::load(host, &today);
    // Found only when an account is due: on macOS a look starts a login shell.
    let mut tools: Option<(Option<std::path::PathBuf>, Vec<std::path::PathBuf>)> = None;
    let mut judged = Vec::new();
    let mut started_any = false;
    for account in &tool.accounts {
        if !settings.warm_up.contains(&account.name) {
            continue;
        }
        // Asked again, not taken from the snapshot: an automatic switch this
        // round, or the user, may have changed the login in use since. The
        // live login decides, as in the window; the record only when the
        // login can't be read.
        let in_use = match engine().live(ProviderId::ClaudeCode) {
            Ok(Live::Account(name)) => name == account.name,
            Ok(_) => false,
            Err(_) => engine().state().is_ok_and(|s| {
                s.active.get(ProviderId::ClaudeCode.as_str()) == Some(&account.name)
            }),
        };
        let account = &model::AccountView {
            in_use,
            ..account.clone()
        };
        let token = warm::token_for(host, &account.name, in_use);
        let at = now();
        // The account in use lends its token as it is; a parked one lends its
        // whole login, which the warm-up renews when it can.
        let expired = match &token {
            None => true,
            Some(t) if in_use => t.lapsed(at),
            Some(t) => t.lapsed(at) && !t.renewable,
        };
        let warmed = ledger.get(&account.name).cloned().unwrap_or_default();
        let mut standing = duty::warm_standing(account, &warmed, expired, in_span, at);
        if standing == WarmStanding::Due {
            let (program, search) = tools.get_or_insert_with(|| {
                let home = real_home();
                (
                    signin::find_program(ProviderId::ClaudeCode, &home),
                    signin::search_path(&home),
                )
            });
            if let (Some(program), Some(token)) = (program.as_ref(), token.as_ref()) {
                show_warm(
                    app,
                    WarmState {
                        name: account.name.clone(),
                        standing: WarmStanding::Warming,
                        warmed_at: warmed.started,
                    },
                );
                let outcome = if in_use {
                    Some(warm::run(program, &token.access, search))
                } else {
                    // No switch while the parked login is lent out: one to
                    // this account would put in use a login being renewed.
                    let state = app.state::<AppState>();
                    let _op = state.ops.lock();
                    let still_parked = engine().state().is_ok_and(|s| {
                        s.active.get(ProviderId::ClaudeCode.as_str()) != Some(&account.name)
                    });
                    still_parked.then(|| warm::run_parked(host, &account.name, program, search))
                };
                let Some(outcome) = outcome else {
                    let unchanged = WarmState {
                        name: account.name.clone(),
                        standing,
                        warmed_at: warmed.started,
                    };
                    show_warm(app, unchanged.clone());
                    judged.push(unchanged);
                    continue;
                };
                // A parked login may be renewed even when the warm-up failed.
                started_any |= outcome.is_ok() || !in_use;
                let name = account.name.clone();
                let warmed = warm::update_if(
                    host,
                    &account.name,
                    &today,
                    now(),
                    outcome.as_ref().copied(),
                    || {
                        engine().state().is_ok_and(|state| {
                            state
                                .accounts
                                .iter()
                                .any(|a| a.provider == ProviderId::ClaudeCode && a.name == name)
                        })
                    },
                );
                ledger.insert(account.name.clone(), warmed.clone());
                standing = duty::warm_standing(account, &warmed, expired, in_span, now());
                show_warm(
                    app,
                    WarmState {
                        name: account.name.clone(),
                        standing: standing.clone(),
                        warmed_at: warmed.started,
                    },
                );
            }
        }
        judged.push(WarmState {
            name: account.name.clone(),
            standing,
            warmed_at: ledger.get(&account.name).and_then(|w| w.started),
        });
    }
    {
        let state = app.state::<AppState>();
        if let Ok(mut inner) = state.inner.lock() {
            inner.warm = judged;
        };
    }
    refresh(app);
    if started_any {
        read_usage(app);
    }
}

/// Seconds until the next usage read, judged from the resets the last
/// reading knows of (they stay where they are whatever the new one says).
fn next_read_wait(app: &AppHandle) -> u64 {
    use pitstop_core::usage::UsageReport;
    let state = app.state::<AppState>();
    let (resets, offline): (Vec<i64>, bool) =
        state
            .inner
            .lock()
            .map(|inner| {
                let resets = inner
                    .usage
                    .iter()
                    .filter_map(|u| match &u.report {
                        UsageReport::Ok(usage) => {
                            Some(usage.windows.iter().filter_map(|w| w.resets_at))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect();
                // No reading came back and some failed for want of a network, as
                // when the app starts at login before Wi-Fi is up.
                let offline = !inner.usage.iter().any(|u| matches!(u.report, UsageReport::Ok(_)))
                && inner.usage.iter().any(
                    |u| matches!(&u.report, UsageReport::Failed(why) if why.starts_with("network")),
                );
                (resets, offline)
            })
            .unwrap_or_default();
    if offline {
        return duty::READ_OFFLINE as u64;
    }
    duty::next_read_after(resets.into_iter(), now()).max(1) as u64
}

/// Look for each tool not found yet (all of them the first time), so one
/// installed while the app runs can be added without a restart; `now` skips
/// the pause between looks. True when one turned up.
pub fn look_for_tools(app: &AppHandle, now: bool) -> bool {
    let state = app.state::<AppState>();
    let missing: Vec<ProviderId> = match state.inner.lock() {
        Ok(inner) => ProviderId::ALL
            .iter()
            .copied()
            .filter(|p| !inner.installed.iter().any(|(q, found)| q == p && *found))
            .collect(),
        Err(_) => return false,
    };
    if missing.is_empty() {
        return false;
    }
    // On macOS a look starts a login shell: not more than twice a minute.
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if !now && last.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) {
            return false;
        }
        *last = Some(Instant::now());
    }
    let home = real_home();
    let found: Vec<(ProviderId, bool)> = missing
        .into_iter()
        .map(|p| (p, signin::find_program(p, &home).is_some()))
        .collect();
    let Ok(mut inner) = state.inner.lock() else {
        return false;
    };
    let mut turned_up = false;
    for (p, there) in found {
        match inner.installed.iter_mut().find(|(q, _)| *q == p) {
            Some(entry) => {
                turned_up |= there && !entry.1;
                entry.1 = there;
            }
            None => inner.installed.push((p, there)),
        }
    }
    turned_up
}

/// Look for the tools once, read usage now, again shortly after each limit
/// resets, and at least every five minutes.
pub fn start_background(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        refresh(&handle);
        look_for_tools(&handle, true);
        refresh(&handle);
    });

    let handle = app.clone();
    std::thread::spawn(move || loop {
        read_usage(&handle);
        std::thread::sleep(Duration::from_secs(next_read_wait(&handle)));
    });

    let handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(DUTIES_EVERY);
        run_duties(&handle);
    });
}

/// The title bar is drawn by the system, not the page, so it follows the
/// chosen theme here; none hands it back to the system's.
pub fn apply_window_theme(app: &AppHandle, choice: Option<&str>) {
    let theme = match choice {
        Some("light") => Some(tauri::Theme::Light),
        Some("dark") => Some(tauri::Theme::Dark),
        _ => None,
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_theme(theme);
    }
}

pub fn show_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The window goes, the app stays in the tray; on macOS the Dock icon goes
/// with the window.
pub fn hide_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

/// Bring the window up with one of its dialogs open, for the tray menu.
pub fn open_dialog(app: &AppHandle, kind: &str, provider: Option<&str>, name: Option<&str>) {
    show_window(app);
    let _ = app.emit(
        "open-dialog",
        serde_json::json!({ "kind": kind, "provider": provider, "name": name }),
    );
}
