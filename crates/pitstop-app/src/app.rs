//! The app's running state: the last usage read, the snapshot built from it,
//! and the one place that rebuilds both the window and the tray menu.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pitstop_core::activity::{self, verb, By, Entry};
use pitstop_core::engine::{AccountUsage, Renewal};
use pitstop_core::live::Live;
use pitstop_core::usage::Http;
use pitstop_core::{signin, Engine, ProviderId, RealHost};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

use crate::duty::{self, WarmStanding};
use crate::i18n::Lang;
use crate::model::{self, Snapshot, WarmState};
use crate::notified::Notified;
use crate::toast::{self, Act};
use crate::{renew, settings, warm};

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
    /// The switches this run made, as logged, newest last: they count with
    /// the log's for the switch before the threshold, even when the log
    /// couldn't be written.
    switches: Vec<Entry>,
    /// The last renewal outcome logged per account: a refusal or failure
    /// retried again and again is logged once, not on every try.
    renewals_logged: HashMap<String, &'static str>,
    /// The notices posted, also before a restart; read on first use.
    notified: Option<Notified>,
}

/// How many of this run's switches [`Automatic::switches`] keeps.
const SWITCHES_KEPT: usize = 100;

impl Automatic {
    /// The notices posted, read from disk the first time, without those
    /// whose time has passed.
    fn notified(&mut self, host: &RealHost, now: i64) -> &mut Notified {
        let notified = self.notified.get_or_insert_with(|| Notified::load(host));
        notified.prune(now);
        notified
    }

    fn switched(&mut self, entry: Entry) {
        if self.switches.len() >= SWITCHES_KEPT {
            self.switches.remove(0);
        }
        self.switches.push(entry);
    }
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
    renewals: renew::Failures,
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

/// Renew the parked Claude Code logins whose token has lapsed, so the read
/// that follows can read them; switches wait meanwhile.
fn renew_parked(app: &AppHandle) {
    let state = app.state::<AppState>();
    // Never `inner` held while waiting for `ops`.
    let failures = match state.inner.lock() {
        Ok(inner) => inner.renewals.clone(),
        Err(_) => return,
    };
    let at = now();
    let renewed = {
        let _op = state.ops.lock();
        engine().renew_parked(&Http::new(), &|name| failures.due(name, at))
    };
    let Ok(renewed) = renewed else {
        return;
    };
    let host = host();
    for (name, renewal) in &renewed {
        let result = match renewal {
            Renewal::Renewed => activity::DONE,
            Renewal::Refused => "refused",
            Renewal::Failed(_) => "failed",
        };
        let repeated = state.automatic.lock().is_ok_and(|mut auto| {
            auto.renewals_logged.insert(name.clone(), result) == Some(result)
                && result != activity::DONE
        });
        if !repeated {
            log(
                &host,
                Entry::new(now(), verb::RENEW, By::App)
                    .provider(ProviderId::ClaudeCode)
                    .account(name)
                    .result(result),
            );
        }
    }
    if let Ok(mut inner) = state.inner.lock() {
        inner.renewals.note(&renewed, now());
    };
}

/// Add a change to the activity log; one that can't be written stops nothing.
pub fn log(host: &RealHost, entry: Entry) {
    let _ = activity::record(host, &entry);
}

/// The account `provider` is using now: its login's owner, or the record
/// when the login can't be read. Not the window's last look, which a switch
/// from the command line may have made out of date.
pub fn in_use_name(provider: ProviderId) -> Option<String> {
    match engine().live(provider) {
        Ok(Live::Account(name)) => Some(name),
        Ok(_) => None,
        Err(_) => engine()
            .state()
            .ok()
            .and_then(|s| s.active_name(provider).cloned()),
    }
}

/// A switch the user made from `from`: it goes in the log, and `from` is not
/// switched back to before the threshold this week.
pub fn switched_by_hand(app: &AppHandle, provider: ProviderId, from: Option<&str>, to: &str) {
    let entry = Entry::new(now(), verb::USE, By::App)
        .provider(provider)
        .account(to)
        .from(from);
    log(&host(), entry.clone());
    let state = app.state::<AppState>();
    if let Ok(mut auto) = state.automatic.lock() {
        auto.switched(entry);
    };
}

/// Read every account's usage, off the calling thread.
fn start_read(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        refresh(&app);
        // However the read ends, the next one may start.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| renew_parked(&app)));
        let read_now = std::panic::catch_unwind(|| engine().usage(&Http::new()));
        if let Ok(Ok(read)) = &read_now {
            // Recording a plan the read names is a change like any other.
            let state = app.state::<AppState>();
            let _op = state.ops.lock();
            engine().record_plans(read);
        }
        let plan = {
            let state = app.state::<AppState>();
            let Ok(mut inner) = state.inner.lock() else {
                return;
            };
            if let Ok(Ok(usage)) = read_now {
                let usage = inner.renewals.mark_refused(usage);
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
        let plan = claude.and_then(|tool| {
            duty::switch_target(tool, settings.auto_switch_at, settings.use_first, now())
                .or_else(|| early_plan(app, &host, &snapshot, &settings, tool, &HashSet::new()))
        });
        if let Some(plan) = plan {
            auto_switch(app, &host, lang, &plan);
        }
    }
    notices(app, &host, &snapshot, &settings, lang, claude);
    if let Some(tool) = claude {
        warm_ups(app, &host, &settings, tool);
    }
}

/// One notification to post, and the key that keeps it from coming again.
struct Post {
    key: String,
    /// When the key may be forgotten; none keeps it until told otherwise.
    until: Option<i64>,
    title: String,
    body: String,
    button: Option<(String, Act)>,
    tap: Act,
}

/// The notifications the last reading calls for, each posted once, also
/// across restarts: an account in use out of a limit, or at the share of one
/// for a tool not switched by itself, another account whose week resets
/// first, and a login the service no longer takes.
fn notices(
    app: &AppHandle,
    host: &RealHost,
    snapshot: &Snapshot,
    settings: &settings::Settings,
    lang: Lang,
    claude: Option<&model::ToolView>,
) {
    let at = now();
    let state = app.state::<AppState>();
    // What was posted is read under the lock; the rest is judged without it.
    let (sent, notified) = {
        let Ok(mut auto) = state.automatic.lock() else {
            return;
        };
        let notified = auto.notified(host, at).keys();
        let mut sent = auto.sent.clone();
        sent.extend(notified.iter().cloned());
        (sent, notified)
    };
    let tool_name = |id: &str| {
        ProviderId::parse(id)
            .map(|p| p.display_name())
            .unwrap_or_default()
    };
    let mut posts = Vec::new();

    if settings.notify_run_out {
        for notice in duty::run_out_notices(&snapshot.tools, &snapshot.suggestions, &sent, at) {
            let Ok(provider) = ProviderId::parse(notice.provider) else {
                continue;
            };
            posts.push(Post {
                until: Notified::until(duty::key_resets(&notice.key), at),
                key: notice.key,
                title: lang.ran_out(&notice.from, &lang.window(&notice.window)),
                body: lang.still_has_room(&notice.to),
                button: Some((
                    lang.switch_to(&notice.to),
                    Act::Switch {
                        provider,
                        name: notice.to,
                    },
                )),
                tap: Act::Open,
            });
        }
    }

    if settings.notify_near {
        for notice in duty::near_notices(
            &snapshot.tools,
            settings.auto_switch_at,
            settings.use_first,
            settings.auto_switch,
            settings.notify_run_out,
            &sent,
            at,
        ) {
            let Ok(provider) = ProviderId::parse(notice.provider) else {
                continue;
            };
            let window = lang.window(&notice.window);
            let resets_in = notice
                .resets_at
                .filter(|&r| r > at)
                .map(|r| crate::i18n::duration(r - at));
            posts.push(Post {
                until: Notified::until(notice.resets_at, at),
                title: lang.near_title(
                    tool_name(notice.provider),
                    &window,
                    notice.percent >= 100.0,
                ),
                body: lang.near_body(
                    &notice.from,
                    notice.percent,
                    &window,
                    notice.to.as_deref(),
                    resets_in.as_deref(),
                ),
                button: notice
                    .to
                    .map(|to| (lang.switch_to(&to), Act::Switch { provider, name: to })),
                key: notice.key,
                tap: Act::Open,
            });
        }
        // Under "week that resets first" with no automatic switch, the
        // switch it would have made before the share is offered instead,
        // once per account and week.
        if !settings.auto_switch && settings.use_first == duty::UseFirst::ResetsSoonest {
            let posted = duty::resets_first_spent(notified.iter());
            let plan =
                claude.and_then(|tool| early_plan(app, host, snapshot, settings, tool, &posted));
            if let Some((plan, early)) = plan.and_then(|p| p.early.map(|e| (p, e))) {
                let key = duty::resets_first_key(&plan.to, early.to_resets);
                if !sent.contains(&key) {
                    posts.push(Post {
                        key,
                        until: Some(early.to_resets.unwrap_or(at + duty::PERIOD)),
                        title: lang.resets_first_title().into(),
                        body: lang.resets_first_body(
                            &plan.from,
                            &plan.to,
                            early.to_resets.map(|r| week_clock(lang, r)).as_deref(),
                            early.from_resets.map(|r| week_clock(lang, r)).as_deref(),
                        ),
                        button: Some((
                            lang.switch_to(&plan.to),
                            Act::Switch {
                                provider: ProviderId::ClaudeCode,
                                name: plan.to,
                            },
                        )),
                        tap: Act::Open,
                    });
                }
            }
        }
    }

    // A refused login is news whatever else is chosen: until it is signed
    // in again, the account can't be switched to.
    let (refused, mended) = duty::sign_in_notices(&snapshot.tools, &notified);
    for (provider, name) in refused {
        let Ok(pid) = ProviderId::parse(provider) else {
            continue;
        };
        let again = Act::SignInAgain {
            provider: pid,
            name: name.clone(),
        };
        posts.push(Post {
            key: duty::sign_in_key(provider, &name),
            until: None,
            title: lang.sign_in_lost_title().into(),
            body: lang.sign_in_lost_body(pid.display_name(), &name),
            button: Some((lang.sign_in_again().into(), again.clone())),
            tap: again,
        });
    }

    if posts.is_empty() && mended.is_empty() {
        return;
    }
    {
        let Ok(mut auto) = state.automatic.lock() else {
            return;
        };
        for post in &posts {
            auto.sent.insert(post.key.clone());
        }
        let notified = auto.notified(host, at);
        for key in &mended {
            notified.remove(key);
        }
        for post in &posts {
            notified.insert(post.key.clone(), post.until);
        }
        notified.save(host);
    }
    for post in posts {
        let button = post
            .button
            .as_ref()
            .map(|(label, act)| (label.as_str(), act.clone()));
        toast::post(app, &post.title, &post.body, button, post.tap);
    }
}

/// A switch before the threshold to the account whose week resets first,
/// when that order is chosen: only on a recent reading, and not within a few
/// minutes of any switch, from here or from the command line. `also_spent`
/// holds more accounts to leave out, by [`duty::early_key`].
fn early_plan(
    app: &AppHandle,
    host: &RealHost,
    snapshot: &Snapshot,
    settings: &settings::Settings,
    tool: &model::ToolView,
    also_spent: &HashSet<String>,
) -> Option<duty::SwitchPlan> {
    if settings.use_first != duty::UseFirst::ResetsSoonest {
        return None;
    }
    let at = now();
    if !snapshot
        .updated_at
        .is_some_and(|read| at - read <= duty::KEEP_GOOD_FOR)
    {
        return None;
    }
    // The log, and this run's switches should the log have missed any.
    let mut log = activity::recent(host, activity::KEEP).unwrap_or_default();
    {
        let state = app.state::<AppState>();
        let auto = state.automatic.lock().ok()?;
        let missed: Vec<Entry> = auto
            .switches
            .iter()
            .filter(|s| !log.contains(s))
            .cloned()
            .collect();
        log.extend(missed);
    }
    log.sort_by_key(|e| std::cmp::Reverse(e.at));
    if activity::last_switch_at(&log).is_some_and(|last| at - last < duty::EARLY_QUIET) {
        return None;
    }
    let mut spent = duty::early_spent(&log, tool, at);
    spent.extend(also_spent.iter().cloned());
    duty::early_target(tool, settings.auto_switch_at, &spent, at)
}

/// Why an automatic switch was made, for the log: `limit|5h|92`, or
/// `resetsFirst|<to's reset>|<from's reset>`.
fn switch_detail(plan: &duty::SwitchPlan) -> String {
    match plan.early {
        Some(early) => format!(
            "resetsFirst|{}|{}",
            early.to_resets.unwrap_or(0),
            early.from_resets.unwrap_or(0)
        ),
        None => format!("limit|{}|{:.0}", plan.limit, plan.percent.floor()),
    }
}

fn auto_switch(app: &AppHandle, host: &RealHost, lang: Lang, plan: &duty::SwitchPlan) {
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
    let entry = Entry::new(now(), verb::AUTO_SWITCH, By::App)
        .provider(ProviderId::ClaudeCode)
        .account(&plan.to)
        .from(Some(&plan.from))
        .detail(switch_detail(plan));
    let switched = crate::commands::switch(app, ProviderId::ClaudeCode, &plan.to, false, false);
    match &switched {
        Ok(done) if done.switched => log(host, entry.clone()),
        Err(error) => log(host, entry.clone().result(error.code)),
        Ok(_) => {}
    }
    match switched {
        Ok(done) if done.switched => {
            if let Ok(mut auto) = state.automatic.lock() {
                auto.sent.insert(key);
                auto.told_unmanaged = false;
                // Counted, with the log, by `duty::early_spent`.
                auto.switched(entry);
            }
            let title = lang.auto_switched(tool, &plan.to);
            let why = match plan.early {
                Some(early) => lang.auto_switched_early_why(
                    &plan.from,
                    &plan.to,
                    early.to_resets.map(|at| week_clock(lang, at)).as_deref(),
                    early.from_resets.map(|at| week_clock(lang, at)).as_deref(),
                ),
                None => lang.auto_switched_why(&plan.from, plan.percent, &lang.window(&plan.limit)),
            };
            // Undoes this switch only while the account switched to is in use.
            let back = Act::SwitchBack {
                provider: ProviderId::ClaudeCode,
                from: plan.to.clone(),
                to: plan.from.clone(),
            };
            toast::post(
                app,
                &title,
                &why,
                Some((lang.switch_back(), back)),
                Act::Open,
            );
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

/// A week's reset as the notification says it, local time: `Tue 21:00`,
/// `21:00 06/11`.
fn week_clock(lang: Lang, at: i64) -> String {
    let format = if lang.code() == "vi" {
        "%H:%M %d/%m"
    } else {
        "%a %H:%M"
    };
    chrono::DateTime::from_timestamp(at, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
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
                log(
                    host,
                    Entry::new(now(), verb::WARM_UP, By::App)
                        .provider(ProviderId::ClaudeCode)
                        .account(&account.name)
                        .result(match &outcome {
                            Ok(()) => activity::DONE,
                            Err(failure) => failure.code(),
                        }),
                );
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
    use pitstop_core::usage::{UsageReport, NETWORK};
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
                    |u| matches!(&u.report, UsageReport::Failed(why) if why.starts_with(NETWORK)),
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
        // What a sign-in left when the app or the computer stopped under it.
        pitstop_core::signin::sweep_leftovers();
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
