//! What the app does by itself, decided from what it already knows: when a
//! Claude Code account is due a warm-up, which account an automatic switch
//! goes to, and which run-outs have not been notified yet. Asks nobody and
//! writes nothing; `app.rs` acts on the answers.

use std::collections::HashSet;

use pitstop_core::engine::AccountUsage;
use pitstop_core::usage::UsageReport;
use serde::{Deserialize, Serialize};

use crate::model::{AccountView, Suggestion, ToolView, UsageView, WindowView};

/// How long a reading that worked stands in for reads that failed.
pub const KEEP_GOOD_FOR: i64 = 15 * 60;

/// When a read asked for at `now` may start: now, or once `READ_SOONEST`
/// has passed since the last one started at `tried_at`.
pub fn read_start(tried_at: Option<i64>, now: i64) -> i64 {
    tried_at.map_or(now, |t| (t + READ_SOONEST).max(now))
}

/// Where usage reads stand. Every read asks for every account, and asking
/// more often than every `READ_SOONEST` gets reads turned away, so a read
/// asked for sooner waits for its turn and one asked for while another runs
/// follows it. None is dropped, and asks that pile up are served by one read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reads {
    pub reading: bool,
    /// When the read waiting for its turn starts.
    pub queued: Option<i64>,
    /// When the last read started, worked or not.
    pub tried_at: Option<i64>,
    /// Asked for while a read ran, and whether without waiting.
    pub again: Option<bool>,
}

/// What to do about a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPlan {
    Nothing,
    Start,
    /// Wait until then, then ask [`Reads::turn_came`].
    WaitUntil(i64),
}

impl Reads {
    /// Whether a read runs or waits for its turn.
    pub fn busy(&self) -> bool {
        self.reading || self.queued.is_some()
    }

    /// A read asked for at `now`; `now_anyway` (an account just added, with
    /// no reading at all) skips the wait.
    pub fn ask(&mut self, now_anyway: bool, now: i64) -> ReadPlan {
        if self.reading {
            self.again = Some(self.again.unwrap_or(false) || now_anyway);
            return ReadPlan::Nothing;
        }
        if self.queued.is_some() {
            if !now_anyway {
                return ReadPlan::Nothing;
            }
            // Served now: the waiting read finds nothing left to do.
            self.queued = None;
        }
        let start = if now_anyway {
            now
        } else {
            read_start(self.tried_at, now)
        };
        if start > now {
            self.queued = Some(start);
            ReadPlan::WaitUntil(start)
        } else {
            self.reading = true;
            self.tried_at = Some(now);
            ReadPlan::Start
        }
    }

    /// The turn of the read waiting until `due` came at `now`.
    pub fn turn_came(&mut self, due: i64, now: i64) -> ReadPlan {
        if self.queued != Some(due) {
            return ReadPlan::Nothing; // served meanwhile
        }
        self.queued = None;
        if self.reading {
            self.again = Some(self.again.unwrap_or(false));
            return ReadPlan::Nothing;
        }
        self.reading = true;
        self.tried_at = Some(now);
        ReadPlan::Start
    }

    /// The running read ended at `now`, or never got going: what was asked
    /// for meanwhile follows.
    pub fn ended(&mut self, now: i64) -> ReadPlan {
        self.reading = false;
        match self.again.take() {
            None => ReadPlan::Nothing,
            Some(now_anyway) => self.ask(now_anyway, now),
        }
    }
}

/// What a read that came back at `now` leaves: the readings, failed ones
/// standing in for by the last that worked while that is recent, and when
/// usage was last read. A reading kept from before stays dated from before,
/// so `updated_at` only moves when nothing was kept; every reading kept is
/// then from a read at or after it.
pub fn merge_read(
    old: &[AccountUsage],
    updated_at: Option<i64>,
    new: Vec<AccountUsage>,
    now: i64,
) -> (Vec<AccountUsage>, Option<i64>) {
    let recent = updated_at.is_some_and(|at| now - at <= KEEP_GOOD_FOR);
    let (usage, kept) = if recent {
        keep_last_good(old, new)
    } else {
        (new, false)
    };
    (usage, if kept { updated_at } else { Some(now) })
}

/// A new usage read, where an account's reading failed (rate limited,
/// offline) keeping the last one that worked: a passing refusal of the usage
/// service is no reason to forget the numbers. Also says whether any was
/// kept, so the read is not taken as a fresh one.
pub fn keep_last_good(old: &[AccountUsage], new: Vec<AccountUsage>) -> (Vec<AccountUsage>, bool) {
    let mut kept = false;
    let usage = new
        .into_iter()
        .map(|read| {
            if !matches!(read.report, UsageReport::Failed(_)) {
                return read;
            }
            match old.iter().find(|o| {
                o.provider == read.provider
                    && o.name == read.name
                    && matches!(o.report, UsageReport::Ok(_))
            }) {
                Some(good) => {
                    kept = true;
                    good.clone()
                }
                None => read,
            }
        })
        .collect();
    (usage, kept)
}

/// A five-hour period, in seconds.
pub const PERIOD: i64 = 5 * 3600;
/// A period starts at the start of the ten minutes its first request fell in.
const START_STEP: i64 = 600;
/// Failed warm-ups of one account a day before it is left until tomorrow.
pub const WARM_TRIES: u32 = 3;
/// How long after a failed warm-up the next one waits.
pub const WARM_RETRY_AFTER: i64 = 10 * 60;
/// The longest wait between warm-ups that keep failing to reach the service.
pub const WARM_RETRY_MOST: i64 = 60 * 60;

/// How long after its last failure `warmed` waits: each failure in a row to
/// reach the service doubles the wait, up to an hour, so a day offline
/// doesn't keep a parked login lent out every ten minutes.
pub fn warm_retry_after(warmed: &Warmed) -> i64 {
    let doublings = warmed.unreachable.saturating_sub(1).min(4);
    (WARM_RETRY_AFTER << doublings).min(WARM_RETRY_MOST)
}
/// Automatic switches tried for one limit of one account before its reset.
pub const SWITCH_TRIES: u32 = 3;
/// How often usage is read when no reset is near.
pub const READ_EVERY: i64 = 5 * 60;
/// The soonest one read follows another.
pub const READ_SOONEST: i64 = 30;
/// How long after a reset its fresh reading is asked for.
const AFTER_RESET: i64 = 30;

/// Seconds until the next read when every read failed for want of a network:
/// sooner than the usual five minutes, so usage shows soon after it is back.
pub const READ_OFFLINE: i64 = 60;

/// Seconds until the next usage read: shortly after the nearest reset still
/// to come, so a limit that starts afresh shows it within a minute, and
/// otherwise every five minutes.
pub fn next_read_after(resets: impl Iterator<Item = i64>, now: i64) -> i64 {
    resets
        .filter(|&at| at > now)
        .map(|at| at + AFTER_RESET - now)
        .min()
        .map_or(READ_EVERY, |wait| wait.clamp(READ_SOONEST, READ_EVERY))
}

/// The share of a limit used as it stands at `now`: one past its reset has
/// started afresh, whatever the last reading said.
pub fn used_now(w: &WindowView, now: i64) -> f64 {
    if w.resets_at.is_some_and(|at| at <= now) {
        0.0
    } else {
        w.percent
    }
}

/// `HH:MM` as minutes after midnight.
fn minutes(clock: &str) -> Option<u32> {
    let (h, m) = clock.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Whether `clock` is a time of day written `HH:MM`.
pub fn is_clock(clock: &str) -> bool {
    clock.len() == 5 && minutes(clock).is_some()
}

/// Whether `now` (minutes after midnight) falls in the span `from`–`to`. A
/// span whose start is later than its end runs past midnight; one that starts
/// where it ends, or that cannot be read, is the whole day.
pub fn in_span(now: u32, from: &str, to: &str) -> bool {
    match (minutes(from), minutes(to)) {
        (Some(from), Some(to)) if from < to => from <= now && now < to,
        (Some(from), Some(to)) if from > to => now >= from || now < to,
        _ => true,
    }
}

/// What happened to one account's warm-ups, kept in `warmed.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Warmed {
    /// The local day `failures` counts for, `2026-10-15`.
    pub date: String,
    pub failures: u32,
    pub last_failure: Option<i64>,
    /// When a warm-up last started its period, epoch seconds.
    pub started: Option<i64>,
    /// What the last failed warm-up came to (`loginRefused`…), until one works.
    pub last_error: Option<String>,
    /// Failures in a row to reach the service; they use none of `failures`.
    pub unreachable: u32,
}

/// Where an account stands as far as warming it up goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum WarmStanding {
    Due,
    /// A warm-up is running now; a parked account can't be switched to
    /// until it ends. Set by the app, never judged here.
    Warming,
    /// Its five-hour period runs until `until`.
    Running {
        until: i64,
    },
    OutsideSpan,
    NoReading,
    NoFiveHour,
    /// The parked login it would be made with has lapsed: signing in again mends it.
    LoginExpired,
    /// The service refused the login: signing in again mends it.
    LoginRefused,
    /// The login in use has lapsed; Claude Code renews it the next time it runs.
    Renewing,
    /// Another limit is used up until `until`: a warm-up would be refused.
    LimitUsedUp {
        until: i64,
    },
    /// As many warm-ups as are made a day failed today.
    GaveUp,
    /// One failed a moment ago, for `reason`; the next waits until `until`.
    Waiting {
        until: i64,
        reason: Option<String>,
    },
}

/// The five-hour limit of a reading, where there is one.
fn five_hour(windows: &[WindowView]) -> Option<&WindowView> {
    windows.iter().find(|w| w.label == "5h")
}

/// Where `account` stands for a warm-up at `now`. `warmed` is what was tried
/// today, `login_expired` whether the login it would be made with has lapsed,
/// `in_span` whether now is inside the warm-up span.
pub fn warm_standing(
    account: &AccountView,
    warmed: &Warmed,
    login_expired: bool,
    in_span: bool,
    now: i64,
) -> WarmStanding {
    let windows = match &account.usage {
        Some(UsageView::Ok { windows }) => Some(windows),
        // The reading of the account in use is made with Claude Code's own
        // login, which Claude Code renews when it runs.
        Some(UsageView::Expired) if account.in_use => return WarmStanding::Renewing,
        // A parked login can't be read until renewed: before a read, or by
        // the warm-up. Its limits are learnt after.
        Some(UsageView::Expired) if login_expired => return WarmStanding::LoginExpired,
        Some(UsageView::Expired) => None,
        Some(UsageView::Unauthorized) => return WarmStanding::LoginRefused,
        None | Some(UsageView::Failed { .. }) => return WarmStanding::NoReading,
    };
    if let Some(windows) = windows {
        let Some(period) = five_hour(windows) else {
            return WarmStanding::NoFiveHour;
        };
        if let Some(until) = period.resets_at.filter(|&at| at > now) {
            return WarmStanding::Running { until };
        }
    }
    // The reading may not show a period a warm-up started minutes ago.
    if let Some(started) = warmed.started {
        let until = started - started.rem_euclid(START_STEP) + PERIOD;
        if until > now {
            return WarmStanding::Running { until };
        }
    }
    if let Some(until) = windows
        .into_iter()
        .flatten()
        .filter(|w| w.percent >= 100.0)
        .filter_map(|w| w.resets_at)
        .filter(|&at| at > now)
        .max()
    {
        return WarmStanding::LimitUsedUp { until };
    }
    if warmed.failures >= WARM_TRIES {
        return WarmStanding::GaveUp;
    }
    if let Some(until) = warmed
        .last_failure
        .map(|at| at + warm_retry_after(warmed))
        .filter(|&until| until > now)
    {
        return WarmStanding::Waiting {
            until,
            reason: warmed.last_error.clone(),
        };
    }
    if login_expired {
        return if account.in_use {
            WarmStanding::Renewing
        } else {
            WarmStanding::LoginExpired
        };
    }
    if !in_span {
        return WarmStanding::OutsideSpan;
    }
    WarmStanding::Due
}

/// Which account an automatic switch goes to, and the one offered once an
/// account runs out, among those with room.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UseFirst {
    /// The one with the most room in the limit that ran low.
    #[default]
    MostLeft,
    /// The one whose week resets first: what an account leaves of its week
    /// is lost at its reset, so that is spent first. One without a weekly
    /// limit (a Team seat) loses what it leaves every five hours, and comes
    /// before them all.
    ResetsSoonest,
}

/// Resets less than this far apart count as one.
pub const SAME_RESET: i64 = 60;

/// How long after any switch, by hand or by itself, the app makes none
/// before a limit is reached.
pub const EARLY_QUIET: i64 = 5 * 60;

/// An automatic switch to make: Claude Code's account in use, `from`, has
/// reached the threshold on `limit`, and `to` comes first among the others;
/// or, with `early`, no limit is reached yet but `to`'s week resets first.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchPlan {
    pub from: String,
    pub to: String,
    pub limit: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
    pub early: Option<Early>,
}

/// A switch before the threshold, to spend first the week that resets first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Early {
    /// When `to`'s week resets; none when it has no weekly limit.
    pub to_resets: Option<i64>,
    /// When `from`'s week resets, where known.
    pub from_resets: Option<i64>,
}

impl SwitchPlan {
    /// Names this limit until it resets, to count attempts and notices by;
    /// an early switch is named by the account and week it goes to.
    pub fn key(&self, provider: &str) -> String {
        match self.early {
            Some(early) => format!(
                "{provider}|{}|early|{}|{}",
                self.from,
                self.to,
                early.to_resets.unwrap_or(0)
            ),
            None => format!(
                "{provider}|{}|{}|{}",
                self.from,
                self.limit,
                self.resets_at.unwrap_or(0)
            ),
        }
    }
}

/// Names an account's week, for the switches made to it before the
/// threshold: one per week, and none once the user leaves it.
pub fn early_key(name: &str, week_resets: Option<i64>) -> String {
    format!("{name}|{}", week_resets.unwrap_or(0))
}

/// The weekly limit for all models.
fn week(windows: &[WindowView]) -> Option<&WindowView> {
    windows.iter().find(|w| w.label == "week")
}

/// Where an account's week stands for ordering by reset: none at all comes
/// first, then a known reset, then one past or unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeekReset {
    None,
    At(i64),
    Unknown,
}

fn week_reset(windows: &[WindowView], now: i64) -> WeekReset {
    match week(windows) {
        None => WeekReset::None,
        Some(w) => match w.resets_at.filter(|&at| at > now) {
            Some(at) => WeekReset::At(at),
            None => WeekReset::Unknown,
        },
    }
}

impl WeekReset {
    fn at(self) -> Option<i64> {
        match self {
            WeekReset::At(at) => Some(at),
            _ => None,
        }
    }

    fn order(self, other: Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let class = |w: Self| match w {
            WeekReset::None => 0,
            WeekReset::At(_) => 1,
            WeekReset::Unknown => 2,
        };
        match (self, other) {
            (WeekReset::At(a), WeekReset::At(b)) if (a - b).abs() < SAME_RESET => Ordering::Equal,
            (WeekReset::At(a), WeekReset::At(b)) => a.cmp(&b),
            _ => class(self).cmp(&class(other)),
        }
    }
}

/// An account that could be switched to, and what orders it.
struct Candidate<'a> {
    account: &'a AccountView,
    /// Its use of the limit being ordered by; none of it counts as unused.
    used: f64,
    /// Its fullest other limit.
    fullest: f64,
    week: WeekReset,
}

fn candidate<'a>(account: &'a AccountView, limit: &str, now: i64) -> Option<Candidate<'a>> {
    let windows = windows_of(account)?;
    Some(Candidate {
        account,
        used: windows
            .iter()
            .find(|w| w.label == limit)
            .map_or(0.0, |w| used_now(w, now)),
        fullest: windows
            .iter()
            .filter(|w| w.label != limit)
            .map(|w| used_now(w, now))
            .fold(0.0, f64::max),
        week: week_reset(windows, now),
    })
}

/// The candidate that comes first by `order`.
fn first<'a>(
    candidates: impl Iterator<Item = Candidate<'a>>,
    order: UseFirst,
) -> Option<Candidate<'a>> {
    candidates.min_by(|x, y| {
        let room = x
            .used
            .total_cmp(&y.used)
            .then(x.fullest.total_cmp(&y.fullest));
        match order {
            UseFirst::MostLeft => room,
            UseFirst::ResetsSoonest => x.week.order(y.week).then(room),
        }
    })
}

/// Where Claude Code should go once a limit of the account in use reaches
/// `threshold` percent: by `order`, among the accounts with a reading and
/// every limit under the threshold, so the switch never lands on an account
/// that would have to switch straight back.
pub fn switch_target(
    tool: &ToolView,
    threshold: u8,
    order: UseFirst,
    now: i64,
) -> Option<SwitchPlan> {
    switch_target_holding(tool, threshold, order, &HashSet::new(), now)
}

/// Names a limit of `account` until it resets, as [`SwitchPlan::key`] does
/// for a switch away from it.
pub fn limit_key(provider: &str, account: &str, label: &str, resets_at: Option<i64>) -> String {
    format!("{provider}|{account}|{label}|{}", resets_at.unwrap_or(0))
}

/// What the notices' record keeps a held limit's [`limit_key`] under.
pub const HELD: &str = "held|";

/// A limit an account was at the threshold of when the user switched to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldLimit {
    /// By [`limit_key`].
    pub key: String,
    pub label: String,
    pub resets_at: Option<i64>,
}

/// The limits of `account` at or over `threshold` now: what a switch by hand
/// to it holds the automatic switch off, until each resets. The user chose
/// the account knowing them; a limit that reaches the threshold only later
/// still switches.
pub fn held_limits(
    provider: &str,
    account: &AccountView,
    threshold: u8,
    now: i64,
) -> Vec<HeldLimit> {
    let threshold = f64::from(threshold);
    windows_of(account)
        .into_iter()
        .flatten()
        .filter(|w| used_now(w, now) >= threshold)
        .map(|w| HeldLimit {
            key: limit_key(provider, &account.name, &w.label, w.resets_at),
            label: w.label.clone(),
            resets_at: w.resets_at,
        })
        .collect()
}

/// [`switch_target`] leaving out the limits in `held` (by [`limit_key`]):
/// those of an account the user switched to by hand while they were already
/// at the threshold.
pub fn switch_target_holding(
    tool: &ToolView,
    threshold: u8,
    order: UseFirst,
    held: &HashSet<String>,
    now: i64,
) -> Option<SwitchPlan> {
    let threshold = f64::from(threshold);
    let active = tool.accounts.iter().find(|a| a.in_use)?;
    let full = windows_of(active)?
        .iter()
        .filter(|w| used_now(w, now) >= threshold)
        .filter(|w| !held.contains(&limit_key(tool.id, &active.name, &w.label, w.resets_at)))
        .max_by(|x, y| used_now(x, now).total_cmp(&used_now(y, now)))?;
    let to = first(
        tool.accounts
            .iter()
            .filter(|a| !a.in_use)
            .filter(|a| {
                windows_of(a).is_some_and(|ws| ws.iter().all(|w| used_now(w, now) < threshold))
            })
            .filter_map(|a| candidate(a, &full.label, now)),
        order,
    )?;
    Some(SwitchPlan {
        from: active.name.clone(),
        to: to.account.name.clone(),
        limit: full.label.clone(),
        percent: used_now(full, now),
        resets_at: full.resets_at,
        early: None,
    })
}

/// Under [`UseFirst::ResetsSoonest`], a switch before any limit of the
/// account in use reaches `threshold`: to the first account, by that order,
/// that comes before it (no weekly limit while it has one, or a week that
/// resets over a minute sooner), has every limit under the threshold, has
/// used no more of its five hours, and wasn't switched to early this week or
/// left by hand (`spent`, by [`early_key`]). Never between two accounts
/// without a weekly limit.
pub fn early_target(
    tool: &ToolView,
    threshold: u8,
    spent: &HashSet<String>,
    now: i64,
) -> Option<SwitchPlan> {
    let threshold = f64::from(threshold);
    let active = tool.accounts.iter().find(|a| a.in_use)?;
    let windows = windows_of(active)?;
    if windows.iter().any(|w| used_now(w, now) >= threshold) {
        return None;
    }
    let from_week = week_reset(windows, now);
    if from_week == WeekReset::None {
        return None;
    }
    let five_hours = |ws: &[WindowView]| {
        ws.iter()
            .find(|w| w.label == "5h")
            .map_or(0.0, |w| used_now(w, now))
    };
    let from_five = five_hours(windows);
    let to = first(
        tool.accounts
            .iter()
            .filter(|a| !a.in_use)
            .filter(|a| {
                let Some(ws) = windows_of(a) else {
                    return false;
                };
                let week = week_reset(ws, now);
                let sooner = match (week, from_week) {
                    (WeekReset::None, _) => true,
                    (WeekReset::At(to), WeekReset::At(from)) => to < from - SAME_RESET,
                    _ => false,
                };
                sooner
                    && ws.iter().all(|w| used_now(w, now) < threshold)
                    && five_hours(ws) <= from_five
                    && !spent.contains(&early_key(&a.name, week.at()))
            })
            .filter_map(|a| candidate(a, "5h", now)),
        UseFirst::ResetsSoonest,
    )?;
    Some(SwitchPlan {
        from: active.name.clone(),
        to: to.account.name.clone(),
        limit: "week".into(),
        percent: from_five,
        resets_at: to.week.at(),
        early: Some(Early {
            to_resets: to.week.at(),
            from_resets: from_week.at(),
        }),
    })
}

/// When `account`'s week resets, by its last reading; none without one.
pub fn week_resets(account: &AccountView, now: i64) -> Option<i64> {
    windows_of(account).and_then(|ws| week_reset(ws, now).at())
}

/// A week, in seconds.
const WEEK: i64 = 7 * 86_400;

/// The accounts not to switch to before the threshold, by [`early_key`],
/// read from the activity log so a restart or the command line forgets
/// nothing: those switched to early in their current week, and those the
/// user switched away from in it, unless the account reached the threshold
/// while in use since (it is then switched back to once it has room). An
/// account without a weekly limit counts its last five hours as its week.
pub fn early_spent(
    log: &[pitstop_core::activity::Entry],
    tool: &ToolView,
    now: i64,
) -> HashSet<String> {
    use pitstop_core::activity::verb;
    let mut spent = HashSet::new();
    for account in &tool.accounts {
        let week = week_resets(account, now);
        let since = week.map_or(now - PERIOD, |at| at - WEEK);
        let name = account.name.as_str();
        // Newest first: the last thing that happened in the week decides.
        let last = log
            .iter()
            .filter(|e| e.at >= since && e.worked() && e.provider.as_deref() == Some(tool.id))
            .find_map(|e| {
                let early = e
                    .detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with("resetsFirst|"));
                match e.verb.as_str() {
                    verb::AUTO_SWITCH if e.account.as_deref() == Some(name) && early => Some(true),
                    verb::USE if e.from.as_deref() == Some(name) => Some(true),
                    verb::AUTO_SWITCH if e.from.as_deref() == Some(name) && !early => Some(false),
                    _ => None,
                }
            });
        if last == Some(true) {
            spent.insert(early_key(name, week));
        }
    }
    spent
}

/// Whether the account in use was last switched to by hand, in its current
/// week (its last five hours without a weekly limit): then no switch before
/// the threshold takes Claude Code away from it. A switch the app made
/// since, at the threshold, ends that. `log` is newest first.
pub fn chosen_by_hand(log: &[pitstop_core::activity::Entry], tool: &ToolView, now: i64) -> bool {
    use pitstop_core::activity::verb;
    let Some(active) = tool.accounts.iter().find(|a| a.in_use) else {
        return false;
    };
    let since = week_resets(active, now).map_or(now - PERIOD, |at| at - WEEK);
    log.iter()
        .filter(|e| e.at >= since && e.worked() && e.provider.as_deref() == Some(tool.id))
        .find(|e| e.verb == verb::USE || e.verb == verb::AUTO_SWITCH)
        .is_some_and(|e| e.verb == verb::USE && e.account.as_deref() == Some(active.name.as_str()))
}

fn windows_of(account: &AccountView) -> Option<&[WindowView]> {
    match &account.usage {
        Some(UsageView::Ok { windows }) => Some(windows),
        _ => None,
    }
}

/// A run-out to notify: the account in use of `provider` has used up
/// `window`, and `to` still has room.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub key: String,
    pub provider: &'static str,
    pub from: String,
    pub to: String,
    pub window: String,
}

/// The suggestions not notified yet. `sent` holds the keys already notified
/// or handled by an automatic switch.
pub fn run_out_notices(
    tools: &[ToolView],
    suggestions: &[Suggestion],
    sent: &HashSet<String>,
    now: i64,
) -> Vec<Notice> {
    suggestions
        .iter()
        .filter_map(|s| {
            let tool = tools.iter().find(|t| t.id == s.provider)?;
            let active = tool.accounts.iter().find(|a| a.name == s.from)?;
            let window = windows_of(active)?.iter().find(|w| w.label == s.window)?;
            // A reading from before the limit reset says nothing about now.
            if window.resets_at.is_some_and(|at| at <= now) {
                return None;
            }
            let resets_at = window.resets_at;
            // Same shape as `SwitchPlan::key`, so a switch that handled it counts.
            let key = format!(
                "{}|{}|{}|{}",
                s.provider,
                s.from,
                s.window,
                resets_at.unwrap_or(0)
            );
            (!sent.contains(&key)).then(|| Notice {
                key,
                provider: s.provider,
                from: s.from.clone(),
                to: s.to.clone(),
                window: s.window.clone(),
            })
        })
        .collect()
}

/// A limit of the account in use at the share, for a tool that isn't
/// switched by itself: once per limit and reset, naming the account that
/// comes first among those with room, or none.
#[derive(Debug, Clone, PartialEq)]
pub struct NearNotice {
    pub key: String,
    pub provider: &'static str,
    pub from: String,
    pub window: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
    pub to: Option<String>,
}

/// The near-limit notices due: for each tool whose account in use has a
/// limit at `threshold` or more, unless it is Claude Code and `claude_auto`
/// (the automatic switch handles it). Claude Code's offer follows `order`,
/// the other tools' the most room. A used-up limit names the account the
/// run-out offer (`suggestions`) names, so the two never disagree, and is
/// left to the run-out notice when that one is on (`run_out_too`) and has
/// an account to offer. `sent` holds the keys already notified.
#[allow(clippy::too_many_arguments)]
pub fn near_notices(
    tools: &[ToolView],
    suggestions: &[Suggestion],
    threshold: u8,
    order: UseFirst,
    claude_auto: bool,
    run_out_too: bool,
    sent: &HashSet<String>,
    now: i64,
) -> Vec<NearNotice> {
    let at = f64::from(threshold);
    tools
        .iter()
        .filter_map(|tool| {
            let claude = tool.id == "claude-code";
            if claude && claude_auto {
                return None;
            }
            let active = tool.accounts.iter().find(|a| a.in_use)?;
            let full = windows_of(active)?
                .iter()
                .filter(|w| used_now(w, now) >= at)
                .max_by(|x, y| used_now(x, now).total_cmp(&used_now(y, now)))?;
            let percent = used_now(full, now);
            let order = if claude { order } else { UseFirst::MostLeft };
            let to = if percent >= 100.0 {
                suggestions
                    .iter()
                    .find(|s| s.provider == tool.id && s.from == active.name)
                    .map(|s| s.to.clone())
            } else {
                switch_target(tool, threshold, order, now).map(|plan| plan.to)
            };
            if run_out_too && percent >= 100.0 && to.is_some() {
                return None;
            }
            let key = near_key(tool.id, &active.name, &full.label, full.resets_at);
            (!sent.contains(&key)).then(|| NearNotice {
                key,
                provider: tool.id,
                from: active.name.clone(),
                window: full.label.clone(),
                percent,
                resets_at: full.resets_at,
                to,
            })
        })
        .collect()
}

/// The reset a notice key ends with (`…|<epoch seconds>`), where known.
pub fn key_resets(key: &str) -> Option<i64> {
    key.rsplit('|')
        .next()
        .and_then(|at| at.parse().ok())
        .filter(|&at| at > 0)
}

/// Names a near-limit notice: one per account, limit and reset.
pub fn near_key(provider: &str, from: &str, window: &str, resets_at: Option<i64>) -> String {
    format!("near|{provider}|{from}|{window}|{}", resets_at.unwrap_or(0))
}

/// Names a "resets first" notice: one per account and week. Only the
/// account that comes first is offered; one already offered isn't again,
/// and the next one isn't offered in its place.
pub fn resets_first_key(to: &str, to_resets: Option<i64>) -> String {
    format!("first|{}", early_key(to, to_resets))
}

/// Names a "sign-in no longer works" notice for an account.
pub fn sign_in_key(provider: &str, name: &str) -> String {
    format!("signin|{provider}|{name}")
}

/// The accounts whose login the service refused and that weren't notified
/// yet, and the sign-in notices to forget: those of accounts read fine
/// since, or gone, so a later refusal is notified again.
pub fn sign_in_notices(
    tools: &[ToolView],
    notified: &HashSet<String>,
) -> (Vec<(&'static str, String)>, Vec<String>) {
    let mut due = Vec::new();
    let mut known = HashSet::new();
    let mut fine = Vec::new();
    for tool in tools {
        for account in &tool.accounts {
            let key = sign_in_key(tool.id, &account.name);
            known.insert(key.clone());
            match &account.usage {
                Some(UsageView::Unauthorized) if !notified.contains(&key) => {
                    due.push((tool.id, account.name.clone()));
                }
                Some(UsageView::Ok { .. }) if notified.contains(&key) => fine.push(key),
                _ => {}
            }
        }
    }
    fine.extend(
        notified
            .iter()
            .filter(|k| k.starts_with("signin|") && !known.contains(*k))
            .cloned(),
    );
    (due, fine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LiveView;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn asks_for_reads_are_never_dropped_and_pile_up_into_one() {
        let mut r = Reads::default();
        assert_eq!(r.ask(false, NOW), ReadPlan::Start);
        // asked while it runs: one read follows, after its turn
        assert_eq!(r.ask(false, NOW + 5), ReadPlan::Nothing);
        assert_eq!(r.ask(false, NOW + 6), ReadPlan::Nothing);
        assert!(r.busy());
        let due = NOW + READ_SOONEST;
        assert_eq!(r.ended(NOW + 10), ReadPlan::WaitUntil(due));
        // more asks while it waits are served by it
        assert_eq!(r.ask(false, NOW + 12), ReadPlan::Nothing);
        assert!(r.busy());
        assert_eq!(r.turn_came(due, due), ReadPlan::Start);
        assert_eq!(r.ended(due + 3), ReadPlan::Nothing);
        assert!(!r.busy());
        // long after, a read starts at once
        assert_eq!(r.ask(false, due + 600), ReadPlan::Start);
    }

    #[test]
    fn an_account_just_added_is_read_without_waiting_its_turn() {
        let mut r = Reads::default();
        assert_eq!(r.ask(false, NOW), ReadPlan::Start);
        assert_eq!(r.ended(NOW + 2), ReadPlan::Nothing);
        let due = NOW + READ_SOONEST;
        assert_eq!(r.ask(false, NOW + 3), ReadPlan::WaitUntil(due));
        // a read now serves the waiting one, which then does nothing
        assert_eq!(r.ask(true, NOW + 4), ReadPlan::Start);
        assert_eq!(r.turn_came(due, due), ReadPlan::Nothing);
        // asked while a read runs, it follows that read without waiting
        assert_eq!(r.ask(true, NOW + 5), ReadPlan::Nothing);
        assert_eq!(r.ended(NOW + 6), ReadPlan::Start);
    }

    #[test]
    fn a_turn_that_comes_while_a_read_runs_follows_it() {
        let mut r = Reads::default();
        assert_eq!(r.ask(false, NOW), ReadPlan::Start);
        assert_eq!(r.ended(NOW + 1), ReadPlan::Nothing);
        let due = NOW + READ_SOONEST;
        assert_eq!(r.ask(false, NOW + 2), ReadPlan::WaitUntil(due));
        // a read got going just before the turn came
        r.reading = true;
        r.tried_at = Some(due - 1);
        assert_eq!(r.turn_came(due, due), ReadPlan::Nothing);
        assert_eq!(r.again, Some(false));
        assert_eq!(
            r.ended(due + 1),
            ReadPlan::WaitUntil(due - 1 + READ_SOONEST)
        );
    }

    #[test]
    fn a_read_too_soon_after_the_last_waits_for_its_turn() {
        assert_eq!(read_start(None, NOW), NOW);
        assert_eq!(read_start(Some(NOW - 10), NOW), NOW - 10 + READ_SOONEST);
        assert_eq!(read_start(Some(NOW - READ_SOONEST), NOW), NOW);
        assert_eq!(read_start(Some(NOW - 600), NOW), NOW);
    }

    #[test]
    fn a_kept_reading_stays_dated_from_before_and_only_while_recent() {
        use pitstop_core::usage::{Usage, UsageReport};
        use pitstop_core::ProviderId;
        let read = |report: UsageReport| AccountUsage {
            provider: ProviderId::ClaudeCode,
            name: "ben2".into(),
            report,
        };
        let good = UsageReport::Ok(Usage::default());
        let failed = || UsageReport::Failed(pitstop_core::usage::RATE_LIMITED.into());
        let old = vec![read(good.clone())];

        let at = NOW - 14 * 60;
        let (usage, updated) = merge_read(&old, Some(at), vec![read(failed())], NOW);
        assert_eq!(usage[0].report, good);
        assert_eq!(updated, Some(at), "kept, so not dated now");

        let at = NOW - 15 * 60 - 1;
        let (usage, updated) = merge_read(&old, Some(at), vec![read(failed())], NOW);
        assert_eq!(usage[0].report, failed());
        assert_eq!(updated, Some(NOW));

        let (usage, updated) = merge_read(&old, Some(at), vec![read(good.clone())], NOW);
        assert_eq!(usage[0].report, good);
        assert_eq!(updated, Some(NOW));
        assert_eq!(
            merge_read(&[], None, vec![read(failed())], NOW).1,
            Some(NOW)
        );
    }

    #[test]
    fn a_failed_read_keeps_the_last_reading_that_worked() {
        use pitstop_core::usage::{Usage, UsageReport};
        use pitstop_core::ProviderId;
        let read = |name: &str, report: UsageReport| AccountUsage {
            provider: ProviderId::ClaudeCode,
            name: name.into(),
            report,
        };
        let good = UsageReport::Ok(Usage::default());
        let failed = || UsageReport::Failed(pitstop_core::usage::RATE_LIMITED.into());
        let old = vec![
            read("ben2", good.clone()),
            read("ben3", UsageReport::Expired),
        ];
        let new = vec![
            read("ben2", failed()),
            read("ben3", failed()),
            read("ben1", failed()),
        ];
        let (kept, any) = keep_last_good(&old, new);
        assert!(any);
        assert_eq!(kept[0].report, good);
        // nothing that worked to keep: the failure shows
        assert_eq!(kept[1].report, failed());
        assert_eq!(kept[2].report, failed());
        // a refusal or a lapse is news, not a passing failure
        let (kept, any) = keep_last_good(&old, vec![read("ben2", UsageReport::Unauthorized)]);
        assert!(!any);
        assert_eq!(kept[0].report, UsageReport::Unauthorized);
    }

    fn window(label: &str, percent: f64, resets_in: Option<i64>) -> WindowView {
        WindowView {
            label: label.into(),
            percent,
            resets_at: resets_in.map(|s| NOW + s),
            length_secs: None,
            quiet: false,
            idle: false,
        }
    }

    fn account(name: &str, in_use: bool, usage: Option<UsageView>) -> AccountView {
        AccountView {
            name: name.into(),
            label: None,
            plan: None,
            in_use,
            parked_at: None,
            usage,
        }
    }

    fn ok(windows: Vec<WindowView>) -> Option<UsageView> {
        Some(UsageView::Ok { windows })
    }

    fn claude(accounts: Vec<AccountView>) -> ToolView {
        ToolView {
            id: "claude-code",
            name: "Claude Code",
            installed: Some(true),
            live: LiveView::Account {
                name: "ben1".into(),
            },
            accounts,
        }
    }

    // ---- span

    #[test]
    fn a_daytime_span_holds_its_inside_and_not_its_outside() {
        assert!(in_span(7 * 60, "07:00", "23:00"));
        assert!(in_span(22 * 60 + 59, "07:00", "23:00"));
        assert!(!in_span(23 * 60, "07:00", "23:00"));
        assert!(!in_span(6 * 60 + 59, "07:00", "23:00"));
    }

    #[test]
    fn a_span_past_midnight_holds_both_sides_of_it() {
        assert!(in_span(23 * 60 + 30, "22:00", "02:00"));
        assert!(in_span(60, "22:00", "02:00"));
        assert!(!in_span(12 * 60, "22:00", "02:00"));
    }

    #[test]
    fn only_a_two_digit_hour_and_minute_is_a_clock() {
        assert!(is_clock("07:00"));
        assert!(is_clock("23:59"));
        assert!(!is_clock("24:00"));
        assert!(!is_clock("7:00"));
        assert!(!is_clock("07:60"));
        assert!(!is_clock("late!"));
    }

    #[test]
    fn an_empty_or_unreadable_span_is_the_whole_day() {
        assert!(in_span(3 * 60, "07:00", "07:00"));
        assert!(in_span(3 * 60, "late", "23:00"));
    }

    // ---- warm-up

    fn standing(
        usage: Option<UsageView>,
        warmed: Warmed,
        expired: bool,
        span: bool,
    ) -> WarmStanding {
        warm_standing(&account("ben2", false, usage), &warmed, expired, span, NOW)
    }

    #[test]
    fn a_period_that_reset_is_due_inside_the_span() {
        let usage = ok(vec![
            window("5h", 0.0, None),
            window("week", 10.0, Some(86_400)),
        ]);
        assert_eq!(
            standing(usage.clone(), Warmed::default(), false, true),
            WarmStanding::Due
        );
        let passed = ok(vec![window("5h", 40.0, Some(-60))]);
        assert_eq!(
            standing(passed, Warmed::default(), false, true),
            WarmStanding::Due
        );
        assert_eq!(
            standing(usage, Warmed::default(), false, false),
            WarmStanding::OutsideSpan
        );
    }

    #[test]
    fn a_running_period_is_left_until_it_resets() {
        let usage = ok(vec![window("5h", 30.0, Some(3600))]);
        assert_eq!(
            standing(usage, Warmed::default(), false, true),
            WarmStanding::Running { until: NOW + 3600 }
        );
    }

    #[test]
    fn a_used_up_period_still_runs_until_it_resets() {
        let usage = ok(vec![window("5h", 100.0, Some(600))]);
        assert_eq!(
            standing(usage, Warmed::default(), false, true),
            WarmStanding::Running { until: NOW + 600 }
        );
    }

    #[test]
    fn a_warm_up_that_just_worked_counts_as_a_running_period_before_the_reading_shows_it() {
        let started = NOW - 1200;
        let warmed = Warmed {
            started: Some(started),
            ..Warmed::default()
        };
        let until = started - started.rem_euclid(START_STEP) + PERIOD;
        assert_eq!(
            standing(ok(vec![window("5h", 0.0, None)]), warmed, false, true),
            WarmStanding::Running { until }
        );
    }

    #[test]
    fn what_is_missing_says_why_it_is_not_warmed_up() {
        let fresh = Warmed::default();
        assert_eq!(
            standing(None, fresh.clone(), false, true),
            WarmStanding::NoReading
        );
        assert_eq!(
            standing(
                Some(UsageView::Failed {
                    message: "x".into(),
                    rate_limited: false,
                    offline: false,
                }),
                fresh.clone(),
                false,
                true
            ),
            WarmStanding::NoReading
        );
        assert_eq!(
            standing(
                ok(vec![window("week", 5.0, None)]),
                fresh.clone(),
                false,
                true
            ),
            WarmStanding::NoFiveHour
        );
        assert_eq!(
            standing(ok(vec![window("5h", 0.0, None)]), fresh.clone(), true, true),
            WarmStanding::LoginExpired
        );
        assert_eq!(
            standing(Some(UsageView::Unauthorized), fresh, false, true),
            WarmStanding::LoginRefused
        );
    }

    #[test]
    fn failures_wait_a_while_and_stop_for_the_day_at_the_limit() {
        let usage = ok(vec![window("5h", 0.0, None)]);
        let just_failed = Warmed {
            failures: 1,
            last_failure: Some(NOW - 60),
            ..Warmed::default()
        };
        assert_eq!(
            standing(usage.clone(), just_failed, false, true),
            WarmStanding::Waiting {
                until: NOW - 60 + WARM_RETRY_AFTER,
                reason: None
            }
        );
        let a_while_ago = Warmed {
            failures: 2,
            last_failure: Some(NOW - WARM_RETRY_AFTER),
            ..Warmed::default()
        };
        assert_eq!(
            standing(usage.clone(), a_while_ago, false, true),
            WarmStanding::Due
        );
        let spent = Warmed {
            failures: WARM_TRIES,
            last_failure: Some(NOW - 7200),
            ..Warmed::default()
        };
        assert_eq!(standing(usage, spent, false, true), WarmStanding::GaveUp);
    }

    #[test]
    fn failing_to_reach_the_service_waits_longer_each_time_and_never_gives_up() {
        let usage = ok(vec![window("5h", 0.0, None)]);
        let offline = |unreachable| Warmed {
            unreachable,
            last_failure: Some(NOW),
            last_error: Some("unreachable".into()),
            ..Warmed::default()
        };
        let waits: Vec<i64> = [1, 2, 3, 4, 5, 40]
            .into_iter()
            .map(|n| match standing(usage.clone(), offline(n), false, true) {
                WarmStanding::Waiting { until, .. } => (until - NOW) / 60,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(waits, [10, 20, 40, 60, 60, 60]);
    }

    // ---- automatic switch

    #[test]
    fn a_limit_at_the_threshold_switches_to_the_account_with_most_of_it_left() {
        let tool = claude(vec![
            account(
                "ben1",
                true,
                ok(vec![
                    window("5h", 91.0, Some(600)),
                    window("week", 20.0, Some(86_400)),
                ]),
            ),
            account("ben2", false, ok(vec![window("5h", 72.0, Some(900))])),
            account("ben3", false, ok(vec![window("5h", 100.0, Some(300))])),
            account(
                "ben4",
                false,
                ok(vec![
                    window("5h", 40.0, Some(900)),
                    window("week", 100.0, Some(9000)),
                ]),
            ),
            account("ben5", false, Some(UsageView::Unauthorized)),
        ]);
        let plan = switch_target(&tool, 90, UseFirst::MostLeft, NOW).unwrap();
        assert_eq!(
            (plan.from.as_str(), plan.to.as_str(), plan.limit.as_str()),
            ("ben1", "ben2", "5h")
        );
        assert_eq!(plan.resets_at, Some(NOW + 600));
    }

    #[test]
    fn below_the_threshold_or_with_nobody_left_there_is_no_switch() {
        let below = claude(vec![
            account("ben1", true, ok(vec![window("5h", 89.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 10.0, None)])),
        ]);
        assert_eq!(switch_target(&below, 90, UseFirst::MostLeft, NOW), None);
        let nobody = claude(vec![
            account("ben1", true, ok(vec![window("5h", 95.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 92.0, Some(900))])),
            account("ben3", false, None),
        ]);
        assert_eq!(switch_target(&nobody, 90, UseFirst::MostLeft, NOW), None);
    }

    #[test]
    fn the_fullest_limit_at_the_threshold_decides_and_a_missing_limit_counts_as_unused() {
        let tool = claude(vec![
            account(
                "ben1",
                true,
                ok(vec![
                    window("5h", 91.0, Some(600)),
                    window("week", 97.0, Some(86_400)),
                ]),
            ),
            account(
                "ben2",
                false,
                ok(vec![
                    window("5h", 10.0, None),
                    window("week", 95.0, Some(1000)),
                ]),
            ),
            account("ben3", false, ok(vec![window("5h", 50.0, None)])),
        ]);
        let plan = switch_target(&tool, 90, UseFirst::MostLeft, NOW).unwrap();
        assert_eq!((plan.limit.as_str(), plan.to.as_str()), ("week", "ben3"));
    }

    // ---- run-out notices

    #[test]
    fn a_run_out_is_notified_once_per_limit_and_reset() {
        let tools = vec![claude(vec![
            account("ben1", true, ok(vec![window("5h", 100.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 21.0, None)])),
        ])];
        let suggestions = vec![Suggestion {
            provider: "claude-code",
            from: "ben1".into(),
            to: "ben2".into(),
            window: "5h".into(),
        }];
        let first = run_out_notices(&tools, &suggestions, &HashSet::new(), NOW);
        assert_eq!(first.len(), 1);
        assert_eq!(
            (
                first[0].from.as_str(),
                first[0].to.as_str(),
                first[0].window.as_str()
            ),
            ("ben1", "ben2", "5h")
        );
        let sent: HashSet<String> = first.iter().map(|n| n.key.clone()).collect();
        assert!(run_out_notices(&tools, &suggestions, &sent, NOW).is_empty());
    }

    #[test]
    fn a_run_out_an_automatic_switch_handled_is_not_notified() {
        let tools = vec![claude(vec![
            account("ben1", true, ok(vec![window("5h", 100.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 21.0, None)])),
        ])];
        let suggestions = vec![Suggestion {
            provider: "claude-code",
            from: "ben1".into(),
            to: "ben2".into(),
            window: "5h".into(),
        }];
        let plan = switch_target(&tools[0], 90, UseFirst::MostLeft, NOW).unwrap();
        let handled: HashSet<String> = [plan.key("claude-code")].into();
        assert!(run_out_notices(&tools, &suggestions, &handled, NOW).is_empty());
    }

    // ---- near-limit, resets-first and sign-in notices

    fn tool_of(id: &'static str, accounts: Vec<AccountView>) -> ToolView {
        ToolView {
            id,
            name: id,
            installed: Some(true),
            live: LiveView::LoggedOut,
            accounts,
        }
    }

    #[test]
    fn a_limit_at_the_share_is_notified_once_with_the_account_to_switch_to() {
        let tools = vec![tool_of(
            "codex",
            vec![
                account("main", true, ok(vec![window("5h", 91.0, Some(600))])),
                account("team", false, ok(vec![window("5h", 30.0, None)])),
                account("full", false, ok(vec![window("5h", 95.0, None)])),
            ],
        )];
        let due = near_notices(
            &tools,
            &[],
            90,
            UseFirst::MostLeft,
            false,
            true,
            &HashSet::new(),
            NOW,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(
            (
                due[0].provider,
                due[0].from.as_str(),
                due[0].window.as_str(),
                due[0].to.as_deref()
            ),
            ("codex", "main", "5h", Some("team"))
        );
        assert_eq!(due[0].resets_at, Some(NOW + 600));
        let sent: HashSet<String> = [due[0].key.clone()].into();
        assert!(
            near_notices(&tools, &[], 90, UseFirst::MostLeft, false, true, &sent, NOW).is_empty()
        );
        // Below the share, nothing.
        assert!(near_notices(
            &tools,
            &[],
            95,
            UseFirst::MostLeft,
            false,
            true,
            &HashSet::new(),
            NOW
        )
        .is_empty());
    }

    #[test]
    fn claude_code_is_left_to_the_automatic_switch_and_follows_use_first_otherwise() {
        let tools = vec![claude(vec![
            account(
                "ben1",
                true,
                ok(vec![
                    window("5h", 92.0, Some(600)),
                    window("week", 40.0, Some(5 * 86_400)),
                ]),
            ),
            account(
                "ben2",
                false,
                ok(vec![
                    window("5h", 10.0, None),
                    window("week", 20.0, Some(4 * 86_400)),
                ]),
            ),
            account(
                "ben3",
                false,
                ok(vec![
                    window("5h", 30.0, None),
                    window("week", 60.0, Some(86_400)),
                ]),
            ),
        ])];
        let none = HashSet::new();
        assert!(
            near_notices(&tools, &[], 90, UseFirst::MostLeft, true, true, &none, NOW).is_empty()
        );
        let most = near_notices(&tools, &[], 90, UseFirst::MostLeft, false, true, &none, NOW);
        assert_eq!(most[0].to.as_deref(), Some("ben2"));
        let soonest = near_notices(
            &tools,
            &[],
            90,
            UseFirst::ResetsSoonest,
            false,
            true,
            &none,
            NOW,
        );
        assert_eq!(soonest[0].to.as_deref(), Some("ben3"));
    }

    #[test]
    fn a_used_up_limit_is_left_to_the_run_out_notice_unless_nobody_has_room() {
        let with_room = vec![tool_of(
            "codex",
            vec![
                account("main", true, ok(vec![window("5h", 100.0, Some(600))])),
                account("team", false, ok(vec![window("5h", 30.0, None)])),
            ],
        )];
        let none = HashSet::new();
        let offer = [Suggestion {
            provider: "codex",
            from: "main".into(),
            to: "team".into(),
            window: "5h".into(),
        }];
        assert!(near_notices(
            &with_room,
            &offer,
            90,
            UseFirst::MostLeft,
            false,
            true,
            &none,
            NOW
        )
        .is_empty());
        let due = near_notices(
            &with_room,
            &offer,
            90,
            UseFirst::MostLeft,
            false,
            false,
            &none,
            NOW,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].to.as_deref(), Some("team"));
        let nobody = vec![tool_of(
            "codex",
            vec![
                account("main", true, ok(vec![window("5h", 100.0, Some(600))])),
                account("team", false, ok(vec![window("5h", 97.0, None)])),
            ],
        )];
        let due = near_notices(
            &nobody,
            &[],
            90,
            UseFirst::MostLeft,
            false,
            true,
            &none,
            NOW,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].to, None);
    }

    #[test]
    fn a_used_up_limit_names_the_run_out_offer_never_another_verdict() {
        // At 100% with the share at 90: the run-out offer names ben2 (95%,
        // under 100); the share's own rule would find nobody.
        let tools = vec![tool_of(
            "codex",
            vec![
                account("ben1", true, ok(vec![window("5h", 100.0, Some(600))])),
                account("ben2", false, ok(vec![window("5h", 95.0, None)])),
            ],
        )];
        let offer = [Suggestion {
            provider: "codex",
            from: "ben1".into(),
            to: "ben2".into(),
            window: "5h".into(),
        }];
        let none = HashSet::new();
        // The run-out notice says it: no second, contradicting notice.
        assert!(near_notices(
            &tools,
            &offer,
            90,
            UseFirst::MostLeft,
            false,
            true,
            &none,
            NOW
        )
        .is_empty());
        // Without the run-out notice, this one names the same account.
        let due = near_notices(
            &tools,
            &offer,
            90,
            UseFirst::MostLeft,
            false,
            false,
            &none,
            NOW,
        );
        assert_eq!(due[0].to.as_deref(), Some("ben2"));
    }

    #[test]
    fn a_notice_key_says_when_its_limit_resets() {
        assert_eq!(
            key_resets(&near_key("codex", "main", "5h", Some(NOW))),
            Some(NOW)
        );
        assert_eq!(key_resets(&near_key("codex", "main", "5h", None)), None);
        assert_eq!(key_resets("signin|codex|main"), None);
    }

    #[test]
    fn a_resets_first_notice_is_named_by_its_account_and_week() {
        assert_eq!(
            resets_first_key("ben3", Some(NOW + 86_400)),
            format!("first|{}", early_key("ben3", Some(NOW + 86_400)))
        );
        assert_ne!(
            resets_first_key("ben3", Some(NOW + 86_400)),
            resets_first_key("ben3", Some(NOW + 8 * 86_400))
        );
    }

    #[test]
    fn a_refused_login_is_notified_once_and_forgotten_once_it_reads_again() {
        let refused = vec![claude(vec![
            account("ben1", true, ok(vec![window("5h", 10.0, None)])),
            account("ben3", false, Some(UsageView::Unauthorized)),
        ])];
        let (due, forget) = sign_in_notices(&refused, &HashSet::new());
        assert_eq!(due, vec![("claude-code", "ben3".to_string())]);
        assert!(forget.is_empty());
        let notified: HashSet<String> = [
            sign_in_key("claude-code", "ben3"),
            sign_in_key("claude-code", "gone"),
        ]
        .into();
        let (due, forget) = sign_in_notices(&refused, &notified);
        assert!(due.is_empty());
        // An account no longer there is forgotten.
        assert_eq!(forget, vec![sign_in_key("claude-code", "gone")]);
        let mended = vec![claude(vec![account(
            "ben3",
            false,
            ok(vec![window("5h", 0.0, None)]),
        )])];
        let (_, forget) = sign_in_notices(&mended, &notified);
        assert!(forget.contains(&sign_in_key("claude-code", "ben3")));
    }

    // ---- verify findings

    #[test]
    fn a_switch_never_goes_to_an_account_already_at_the_threshold_on_another_limit() {
        // A at 5h 91% switching to B (week 95%) would bounce straight back.
        let tool = claude(vec![
            account(
                "a",
                true,
                ok(vec![
                    window("5h", 91.0, Some(600)),
                    window("week", 10.0, Some(86_400)),
                ]),
            ),
            account(
                "b",
                false,
                ok(vec![
                    window("5h", 10.0, None),
                    window("week", 95.0, Some(86_400)),
                ]),
            ),
        ]);
        assert_eq!(switch_target(&tool, 90, UseFirst::MostLeft, NOW), None);
    }

    #[test]
    fn a_used_up_weekly_limit_holds_the_warm_up_until_it_resets() {
        let usage = ok(vec![
            window("5h", 0.0, None),
            window("week", 100.0, Some(7200)),
        ]);
        assert_eq!(
            standing(usage, Warmed::default(), false, true),
            WarmStanding::LimitUsedUp { until: NOW + 7200 }
        );
    }

    #[test]
    fn a_failed_warm_up_says_why_while_it_waits() {
        let usage = ok(vec![window("5h", 0.0, None)]);
        let failed = Warmed {
            failures: 1,
            last_failure: Some(NOW - 60),
            last_error: Some("loginRefused".into()),
            ..Warmed::default()
        };
        assert_eq!(
            standing(usage, failed, false, true),
            WarmStanding::Waiting {
                until: NOW - 60 + WARM_RETRY_AFTER,
                reason: Some("loginRefused".into())
            }
        );
    }

    #[test]
    fn the_login_in_use_with_a_lapsed_token_waits_for_claude_code_to_renew_it() {
        let usage = ok(vec![window("5h", 0.0, None)]);
        let in_use = account("ben1", true, usage.clone());
        assert_eq!(
            warm_standing(&in_use, &Warmed::default(), true, true, NOW),
            WarmStanding::Renewing
        );
        let parked = account("ben2", false, usage);
        assert_eq!(
            warm_standing(&parked, &Warmed::default(), true, true, NOW),
            WarmStanding::LoginExpired
        );
    }

    #[test]
    fn a_lapsed_or_refused_login_reads_by_whose_login_it_is() {
        let fresh = Warmed::default();
        let in_use = |usage| account("ben1", true, usage);
        let parked = |usage| account("ben2", false, usage);
        // The reading of the account in use goes out with Claude Code's own
        // login: a lapsed one is renewed the next time Claude Code runs.
        assert_eq!(
            warm_standing(&in_use(Some(UsageView::Expired)), &fresh, false, true, NOW),
            WarmStanding::Renewing
        );
        // A parked login that renews is warmed up unread, which renews it;
        // one that can't waits to be signed in again.
        assert_eq!(
            warm_standing(&parked(Some(UsageView::Expired)), &fresh, false, true, NOW),
            WarmStanding::Due
        );
        assert_eq!(
            warm_standing(&parked(Some(UsageView::Expired)), &fresh, false, false, NOW),
            WarmStanding::OutsideSpan
        );
        assert_eq!(
            warm_standing(&parked(Some(UsageView::Expired)), &fresh, true, true, NOW),
            WarmStanding::LoginExpired
        );
        let warmed = Warmed {
            started: Some(NOW - 60),
            ..Default::default()
        };
        assert!(matches!(
            warm_standing(&parked(Some(UsageView::Expired)), &warmed, false, true, NOW),
            WarmStanding::Running { .. }
        ));
        for account in [
            in_use(Some(UsageView::Unauthorized)),
            parked(Some(UsageView::Unauthorized)),
        ] {
            assert_eq!(
                warm_standing(&account, &fresh, false, true, NOW),
                WarmStanding::LoginRefused
            );
        }
    }

    // ---- readings older than a reset

    #[test]
    fn a_limit_past_its_reset_does_not_switch_or_notify_until_read_again() {
        let tool = claude(vec![
            account("ben1", true, ok(vec![window("5h", 100.0, Some(-60))])),
            account("ben2", false, ok(vec![window("5h", 10.0, None)])),
        ]);
        assert_eq!(switch_target(&tool, 90, UseFirst::MostLeft, NOW), None);
        let suggestions = vec![Suggestion {
            provider: "claude-code",
            from: "ben1".into(),
            to: "ben2".into(),
            window: "5h".into(),
        }];
        assert!(run_out_notices(&[tool], &suggestions, &HashSet::new(), NOW).is_empty());
    }

    #[test]
    fn an_account_whose_full_limit_has_reset_is_a_fair_switch_target() {
        let tool = claude(vec![
            account("ben1", true, ok(vec![window("5h", 95.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 98.0, Some(-30))])),
        ]);
        assert_eq!(
            switch_target(&tool, 90, UseFirst::MostLeft, NOW).map(|p| p.to),
            Some("ben2".to_string())
        );
    }

    // ---- use first: resets soonest

    fn logged(
        at: i64,
        verb: &str,
        to: &str,
        from: &str,
        detail: Option<&str>,
    ) -> pitstop_core::activity::Entry {
        let mut entry =
            pitstop_core::activity::Entry::new(at, verb, pitstop_core::activity::By::App)
                .provider(pitstop_core::ProviderId::ClaudeCode)
                .account(to)
                .from(Some(from));
        if let Some(detail) = detail {
            entry = entry.detail(detail);
        }
        entry
    }

    #[test]
    fn an_account_without_a_week_counts_its_last_five_hours_as_one() {
        use pitstop_core::activity::verb;
        let tool = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            account("seat", false, ok(vec![window("5h", 10.0, Some(3600))])),
        ]);
        let left = |ago| vec![logged(NOW - ago, verb::USE, "ben1", "seat", None)];
        // Left by hand an hour ago: not switched back to early.
        assert_eq!(
            early_spent(&left(3600), &tool, NOW),
            [early_key("seat", None)].into()
        );
        // Six hours ago: its five hours since then were lost anyway.
        assert!(early_spent(&left(6 * 3600), &tool, NOW).is_empty());
    }

    #[test]
    fn the_log_says_which_accounts_were_switched_to_early_or_left_this_week() {
        use pitstop_core::activity::verb;
        let tool = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            weekly("ben2", false, 0.0, 10.0, 2 * 86_400),
            weekly("ben3", false, 0.0, 10.0, 3 * 86_400),
            weekly("ben4", false, 0.0, 10.0, 4 * 86_400),
        ]);
        // Newest first, as the log reads.
        let log = vec![
            // ben3 was switched to early, then reached the threshold in use.
            logged(
                NOW - 100,
                verb::AUTO_SWITCH,
                "ben1",
                "ben3",
                Some("limit|5h|91"),
            ),
            logged(
                NOW - 3_000,
                verb::AUTO_SWITCH,
                "ben3",
                "ben1",
                Some("resetsFirst|1|2"),
            ),
            // The user left ben2 by hand.
            logged(NOW - 4_000, verb::USE, "ben1", "ben2", None),
            // ben4 was switched to early last week: that week is over.
            logged(
                NOW - 5 * 86_400,
                verb::AUTO_SWITCH,
                "ben4",
                "ben1",
                Some("resetsFirst|1|2"),
            ),
            // A switch that failed counts for nothing.
            logged(NOW - 200, verb::USE, "ben3", "ben4", None).result("vaultCorrupt"),
        ];
        let spent = early_spent(&log, &tool, NOW);
        assert_eq!(spent, [early_key("ben2", Some(NOW + 2 * 86_400))].into());
        assert_eq!(
            early_target(&tool, 90, &spent, NOW).map(|p| p.to),
            Some("ben3".to_string())
        );
    }

    fn weekly(name: &str, in_use: bool, five: f64, week_pct: f64, resets_in: i64) -> AccountView {
        account(
            name,
            in_use,
            ok(vec![
                window("5h", five, Some(3600)),
                window("week", week_pct, Some(resets_in)),
            ]),
        )
    }

    #[test]
    fn resets_soonest_switches_to_the_week_that_resets_first_not_the_emptiest() {
        let tool = claude(vec![
            weekly("ben1", true, 92.0, 40.0, 5 * 86_400),
            weekly("ben2", false, 10.0, 20.0, 4 * 86_400),
            weekly("ben3", false, 30.0, 60.0, 86_400),
        ]);
        let most_left = switch_target(&tool, 90, UseFirst::MostLeft, NOW).unwrap();
        assert_eq!(most_left.to, "ben2");
        let soonest = switch_target(&tool, 90, UseFirst::ResetsSoonest, NOW).unwrap();
        assert_eq!(
            (soonest.to.as_str(), soonest.limit.as_str()),
            ("ben3", "5h")
        );
        assert_eq!(soonest.early, None);
    }

    #[test]
    fn resets_soonest_puts_an_account_without_a_week_first_and_an_unknown_reset_last() {
        let tool = claude(vec![
            weekly("ben1", true, 95.0, 40.0, 5 * 86_400),
            account("seat", false, ok(vec![window("5h", 50.0, Some(3600))])),
            weekly("ben3", false, 30.0, 60.0, 86_400),
        ]);
        let plan = switch_target(&tool, 90, UseFirst::ResetsSoonest, NOW).unwrap();
        assert_eq!(plan.to, "seat");

        let unknown = claude(vec![
            weekly("ben1", true, 95.0, 40.0, 5 * 86_400),
            account(
                "ben2",
                false,
                ok(vec![window("5h", 0.0, None), window("week", 10.0, None)]),
            ),
            weekly("ben3", false, 30.0, 60.0, 6 * 86_400),
        ]);
        let plan = switch_target(&unknown, 90, UseFirst::ResetsSoonest, NOW).unwrap();
        assert_eq!(plan.to, "ben3");
    }

    #[test]
    fn resets_a_minute_apart_count_as_one_and_room_decides() {
        let tool = claude(vec![
            weekly("ben1", true, 95.0, 40.0, 5 * 86_400),
            weekly("ben2", false, 60.0, 20.0, 86_400),
            weekly("ben3", false, 10.0, 60.0, 86_400 + 30),
        ]);
        let plan = switch_target(&tool, 90, UseFirst::ResetsSoonest, NOW).unwrap();
        assert_eq!(plan.to, "ben3");
    }

    #[test]
    fn an_early_switch_goes_to_the_week_that_resets_first_with_room() {
        let tool = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            weekly("ben2", false, 0.0, 10.0, 6 * 86_400),
            weekly("ben3", false, 20.0, 70.0, 2 * 86_400),
        ]);
        let plan = early_target(&tool, 90, &HashSet::new(), NOW).unwrap();
        assert_eq!((plan.from.as_str(), plan.to.as_str()), ("ben1", "ben3"));
        assert_eq!(
            plan.early,
            Some(Early {
                to_resets: Some(NOW + 2 * 86_400),
                from_resets: Some(NOW + 5 * 86_400),
            })
        );
        assert!(plan.key("claude-code").contains("|early|ben3|"));
    }

    #[test]
    fn no_early_switch_to_one_used_more_in_five_hours_near_the_threshold_or_spent() {
        // ben3's week resets first, but it has used more of its five hours.
        let busier = claude(vec![
            weekly("ben1", true, 10.0, 30.0, 5 * 86_400),
            weekly("ben3", false, 20.0, 70.0, 2 * 86_400),
        ]);
        assert_eq!(early_target(&busier, 90, &HashSet::new(), NOW), None);
        // ben3's week is at the threshold.
        let full = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            weekly("ben3", false, 20.0, 90.0, 2 * 86_400),
        ]);
        assert_eq!(early_target(&full, 90, &HashSet::new(), NOW), None);
        // Switched to early this week, or left by hand.
        let fresh = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            weekly("ben3", false, 20.0, 70.0, 2 * 86_400),
        ]);
        let spent: HashSet<String> = [early_key("ben3", Some(NOW + 2 * 86_400))].into();
        assert_eq!(early_target(&fresh, 90, &spent, NOW), None);
        // Next week it may be switched to again.
        let spent: HashSet<String> = [early_key("ben3", Some(NOW - 5 * 86_400))].into();
        assert!(early_target(&fresh, 90, &spent, NOW).is_some());
    }

    #[test]
    fn no_early_switch_once_a_limit_is_reached_or_from_an_account_without_a_week() {
        let reached = claude(vec![
            weekly("ben1", true, 92.0, 30.0, 5 * 86_400),
            weekly("ben3", false, 20.0, 70.0, 2 * 86_400),
        ]);
        assert_eq!(early_target(&reached, 90, &HashSet::new(), NOW), None);
        let seat = claude(vec![
            account("seat", true, ok(vec![window("5h", 40.0, Some(3600))])),
            account("seat2", false, ok(vec![window("5h", 0.0, None)])),
            weekly("ben3", false, 0.0, 10.0, 86_400),
        ]);
        assert_eq!(early_target(&seat, 90, &HashSet::new(), NOW), None);
    }

    #[test]
    fn an_account_without_a_week_is_switched_to_early_and_a_minute_sooner_is_not_sooner() {
        let tool = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            account("seat", false, ok(vec![window("5h", 10.0, Some(3600))])),
        ]);
        let plan = early_target(&tool, 90, &HashSet::new(), NOW).unwrap();
        assert_eq!(plan.to, "seat");
        assert_eq!(plan.early.unwrap().to_resets, None);

        let tie = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 86_400),
            weekly("ben2", false, 0.0, 10.0, 86_400 - 30),
        ]);
        assert_eq!(early_target(&tie, 90, &HashSet::new(), NOW), None);
    }

    #[test]
    fn a_limit_held_by_a_switch_by_hand_doesnt_switch_but_a_new_one_does() {
        // The user switched back to ben1 at 92% of its five hours.
        let tool = claude(vec![
            account(
                "ben1",
                true,
                ok(vec![
                    window("5h", 92.0, Some(600)),
                    window("week", 40.0, Some(5 * 86_400)),
                ]),
            ),
            account("ben2", false, ok(vec![window("5h", 10.0, None)])),
        ]);
        let ben1 = &tool.accounts[0];
        let held: HashSet<String> = held_limits("claude-code", ben1, 90, NOW)
            .into_iter()
            .map(|limit| {
                assert_eq!(
                    (limit.label.as_str(), limit.resets_at),
                    ("5h", Some(NOW + 600))
                );
                limit.key
            })
            .collect();
        assert_eq!(
            held,
            [limit_key("claude-code", "ben1", "5h", Some(NOW + 600))].into()
        );
        assert_eq!(
            switch_target_holding(&tool, 90, UseFirst::MostLeft, &held, NOW),
            None
        );
        // The held key is the plan's own: no try is spent on it either.
        let plan = switch_target(&tool, 90, UseFirst::MostLeft, NOW).unwrap();
        assert!(held.contains(&plan.key("claude-code")));

        // Later its week reaches the share too: that one still switches.
        let later = claude(vec![
            account(
                "ben1",
                true,
                ok(vec![
                    window("5h", 95.0, Some(600)),
                    window("week", 91.0, Some(5 * 86_400)),
                ]),
            ),
            account("ben2", false, ok(vec![window("5h", 10.0, None)])),
        ]);
        let plan = switch_target_holding(&later, 90, UseFirst::MostLeft, &held, NOW).unwrap();
        assert_eq!((plan.to.as_str(), plan.limit.as_str()), ("ben2", "week"));
        // Once the five hours reset, the held key names a past period.
        let reset = claude(vec![
            account("ben1", true, ok(vec![window("5h", 93.0, Some(18_600))])),
            account("ben2", false, ok(vec![window("5h", 10.0, None)])),
        ]);
        assert!(switch_target_holding(&reset, 90, UseFirst::MostLeft, &held, NOW).is_some());
    }

    #[test]
    fn an_account_chosen_by_hand_isnt_left_before_the_threshold_this_week() {
        use pitstop_core::activity::verb;
        let tool = claude(vec![
            weekly("ben1", true, 40.0, 30.0, 5 * 86_400),
            weekly("ben3", false, 0.0, 10.0, 2 * 86_400),
        ]);
        // Newest first. The user switched to ben1 an hour ago.
        let by_hand = vec![logged(NOW - 3600, verb::USE, "ben1", "ben3", None)];
        assert!(chosen_by_hand(&by_hand, &tool, NOW));
        // The app's own switch to it doesn't count.
        let by_app = vec![logged(
            NOW - 3600,
            verb::AUTO_SWITCH,
            "ben1",
            "ben3",
            Some("limit|5h|91"),
        )];
        assert!(!chosen_by_hand(&by_app, &tool, NOW));
        // Nor a switch by hand from its last week.
        let last_week = vec![logged(NOW - 3 * 86_400, verb::USE, "ben1", "ben3", None)];
        assert!(!chosen_by_hand(&last_week, &tool, NOW));
        // Nor one that a later switch by the app, at the threshold, followed.
        let then_app = vec![
            logged(
                NOW - 60,
                verb::AUTO_SWITCH,
                "ben1",
                "ben3",
                Some("limit|5h|92"),
            ),
            logged(NOW - 3600, verb::USE, "ben3", "ben1", None),
        ];
        assert!(!chosen_by_hand(&then_app, &tool, NOW));
        // A failed switch says nothing.
        let failed = vec![
            logged(NOW - 60, verb::USE, "ben3", "ben1", None).result("vaultCorrupt"),
            logged(NOW - 3600, verb::USE, "ben1", "ben3", None),
        ];
        assert!(chosen_by_hand(&failed, &tool, NOW));
    }

    #[test]
    fn the_next_read_follows_the_next_reset_closely_but_never_too_often() {
        assert_eq!(next_read_after([NOW + 100].into_iter(), NOW), 130);
        assert_eq!(
            next_read_after([NOW + 3600, NOW + 200].into_iter(), NOW),
            230
        );
        assert_eq!(next_read_after([NOW - 10].into_iter(), NOW), READ_EVERY);
        assert_eq!(next_read_after(std::iter::empty(), NOW), READ_EVERY);
        assert_eq!(next_read_after([NOW + 2].into_iter(), NOW), 32);
        assert_eq!(
            next_read_after([NOW - 5, NOW + 1].into_iter(), NOW),
            READ_SOONEST.max(31)
        );
        assert_eq!(next_read_after([NOW + 1000].into_iter(), NOW), READ_EVERY);
    }
}
