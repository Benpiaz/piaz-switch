//! Renewing parked Claude Code logins before each usage read: which lapsed
//! ones to try, what came of it, and how one Anthropic refused shows. Asks
//! nobody; `app.rs` renews them with `Engine::renew_parked`.

use std::collections::HashMap;

use pitstop_core::engine::{AccountUsage, Renewal};
use pitstop_core::usage::UsageReport;
use pitstop_core::ProviderId;

/// How long after a parked login's renewal failed the next is tried.
pub const RETRY_AFTER: i64 = 10 * 60;
/// How long after one was refused: only signing in again is likely to mend it.
pub const RETRY_REFUSED_AFTER: i64 = 60 * 60;

/// The renewals that came to nothing, by account, while the app runs.
#[derive(Debug, Clone, Default)]
pub struct Failures(HashMap<String, Failure>);

#[derive(Debug, Clone, Copy)]
struct Failure {
    at: i64,
    /// The login can't be renewed; it stays so until a renewal works.
    refused: bool,
}

impl Failures {
    /// Whether `name`'s lapsed login may be tried at `now`.
    pub fn due(&self, name: &str, now: i64) -> bool {
        self.0.get(name).is_none_or(|f| {
            let wait = if f.refused {
                RETRY_REFUSED_AFTER
            } else {
                RETRY_AFTER
            };
            now - f.at >= wait
        })
    }

    /// Write down what the renewals made at `now` came to. A login refused
    /// once stays refused through failures to reach Anthropic after.
    pub fn note(&mut self, renewed: &[(String, Renewal)], now: i64) {
        for (name, outcome) in renewed {
            let refused = match outcome {
                Renewal::Renewed => {
                    self.0.remove(name);
                    continue;
                }
                Renewal::Refused => true,
                Renewal::Failed(_) => self.0.get(name).is_some_and(|f| f.refused),
            };
            self.0.insert(name.clone(), Failure { at: now, refused });
        }
    }

    /// `read` where each Claude Code login that can't be renewed shows as one
    /// to sign in to again, not as one that merely lapsed: no switch or
    /// warm-up would renew it either.
    pub fn mark_refused(&self, read: Vec<AccountUsage>) -> Vec<AccountUsage> {
        read.into_iter()
            .map(|mut usage| {
                if usage.provider == ProviderId::ClaudeCode
                    && usage.report == UsageReport::Expired
                    && self.0.get(&usage.name).is_some_and(|f| f.refused)
                {
                    usage.report = UsageReport::Unauthorized;
                }
                usage
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn read(provider: ProviderId, name: &str, report: UsageReport) -> AccountUsage {
        AccountUsage {
            provider,
            name: name.into(),
            report,
        }
    }

    #[test]
    fn a_failed_renewal_waits_before_the_next_and_a_renewed_one_does_not() {
        let mut failures = Failures::default();
        assert!(failures.due("ben2", NOW));

        failures.note(
            &[
                ("ben2".into(), Renewal::Failed("network: timeout".into())),
                ("ben3".into(), Renewal::Refused),
                ("ben4".into(), Renewal::Renewed),
            ],
            NOW,
        );
        assert!(!failures.due("ben2", NOW + RETRY_AFTER - 1));
        assert!(failures.due("ben2", NOW + RETRY_AFTER));
        assert!(!failures.due("ben3", NOW + RETRY_AFTER));
        assert!(failures.due("ben3", NOW + RETRY_REFUSED_AFTER));
        assert!(failures.due("ben4", NOW));

        failures.note(&[("ben2".into(), Renewal::Renewed)], NOW + RETRY_AFTER);
        assert!(failures.due("ben2", NOW + RETRY_AFTER));
    }

    #[test]
    fn a_refused_login_stays_refused_through_a_failure_to_reach_anthropic() {
        let mut failures = Failures::default();
        failures.note(&[("ben2".into(), Renewal::Refused)], NOW);
        failures.note(
            &[("ben2".into(), Renewal::Failed("network: timeout".into()))],
            NOW + RETRY_REFUSED_AFTER,
        );
        let marked = failures.mark_refused(vec![read(
            ProviderId::ClaudeCode,
            "ben2",
            UsageReport::Expired,
        )]);
        assert_eq!(marked[0].report, UsageReport::Unauthorized);

        failures.note(
            &[("ben2".into(), Renewal::Renewed)],
            NOW + 2 * RETRY_REFUSED_AFTER,
        );
        let marked = failures.mark_refused(vec![read(
            ProviderId::ClaudeCode,
            "ben2",
            UsageReport::Expired,
        )]);
        assert_eq!(marked[0].report, UsageReport::Expired);
    }

    #[test]
    fn only_a_refused_login_still_lapsed_shows_as_one_to_sign_in_again() {
        let mut failures = Failures::default();
        failures.note(
            &[
                ("ben2".into(), Renewal::Refused),
                ("ben3".into(), Renewal::Failed("network: timeout".into())),
            ],
            NOW,
        );
        let marked = failures.mark_refused(vec![
            read(ProviderId::ClaudeCode, "ben2", UsageReport::Expired),
            read(ProviderId::ClaudeCode, "ben3", UsageReport::Expired),
            read(ProviderId::Codex, "ben2", UsageReport::Expired),
        ]);
        let reports: Vec<_> = marked.iter().map(|u| &u.report).collect();
        assert_eq!(
            reports,
            [
                &UsageReport::Unauthorized,
                &UsageReport::Expired,
                &UsageReport::Expired
            ]
        );

        // Signed in again since: its new token reads, and that stands.
        let fresh = failures.mark_refused(vec![read(
            ProviderId::ClaudeCode,
            "ben2",
            UsageReport::Failed("rate limited".into()),
        )]);
        assert_eq!(fresh[0].report, UsageReport::Failed("rate limited".into()));
    }
}
