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
/// Automatic switches tried for one limit of one account before its reset.
pub const SWITCH_TRIES: u32 = 3;
/// How often usage is read when no reset is near.
pub const READ_EVERY: i64 = 5 * 60;
/// The soonest one read follows another.
pub const READ_SOONEST: i64 = 30;
/// How long after a reset its fresh reading is asked for.
const AFTER_RESET: i64 = 30;

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
        // A parked login can't be read until renewed, and the warm-up renews
        // it: its limits are learnt after.
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
        .map(|at| at + WARM_RETRY_AFTER)
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

/// An automatic switch to make: Claude Code's account in use, `from`, has
/// reached the threshold on `limit`, and `to` has the most of it left.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchPlan {
    pub from: String,
    pub to: String,
    pub limit: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
}

impl SwitchPlan {
    /// Names this limit until it resets, to count attempts and notices by.
    pub fn key(&self, provider: &str) -> String {
        format!(
            "{provider}|{}|{}|{}",
            self.from,
            self.limit,
            self.resets_at.unwrap_or(0)
        )
    }
}

/// Where Claude Code should go once a limit of the account in use reaches
/// `threshold` percent: the account with the least of that limit used, among
/// those with a reading and every limit under the threshold, so the switch
/// never lands on an account that would have to switch straight back.
pub fn switch_target(tool: &ToolView, threshold: u8, now: i64) -> Option<SwitchPlan> {
    let threshold = f64::from(threshold);
    let active = tool.accounts.iter().find(|a| a.in_use)?;
    let full = windows_of(active)?
        .iter()
        .filter(|w| used_now(w, now) >= threshold)
        .max_by(|x, y| used_now(x, now).total_cmp(&used_now(y, now)))?;
    let (to, _) = tool
        .accounts
        .iter()
        .filter(|a| !a.in_use)
        .filter_map(|a| {
            let windows = windows_of(a)?;
            if windows.iter().any(|w| used_now(w, now) >= threshold) {
                return None;
            }
            let used = windows
                .iter()
                .find(|w| w.label == full.label)
                .map_or(0.0, |w| used_now(w, now));
            Some((a, used))
        })
        .min_by(|x, y| x.1.total_cmp(&y.1))?;
    Some(SwitchPlan {
        from: active.name.clone(),
        to: to.name.clone(),
        limit: full.label.clone(),
        percent: used_now(full, now),
        resets_at: full.resets_at,
    })
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
        let good = UsageReport::Ok(Usage { windows: vec![] });
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
        let good = UsageReport::Ok(Usage { windows: vec![] });
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
        let plan = switch_target(&tool, 90, NOW).unwrap();
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
        assert_eq!(switch_target(&below, 90, NOW), None);
        let nobody = claude(vec![
            account("ben1", true, ok(vec![window("5h", 95.0, Some(600))])),
            account("ben2", false, ok(vec![window("5h", 92.0, Some(900))])),
            account("ben3", false, None),
        ]);
        assert_eq!(switch_target(&nobody, 90, NOW), None);
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
        let plan = switch_target(&tool, 90, NOW).unwrap();
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
        let plan = switch_target(&tools[0], 90, NOW).unwrap();
        let handled: HashSet<String> = [plan.key("claude-code")].into();
        assert!(run_out_notices(&tools, &suggestions, &handled, NOW).is_empty());
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
        assert_eq!(switch_target(&tool, 90, NOW), None);
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
        assert_eq!(switch_target(&tool, 90, NOW), None);
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
            switch_target(&tool, 90, NOW).map(|p| p.to),
            Some("ben2".to_string())
        );
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
