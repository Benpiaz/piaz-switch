//! Whose login a tool is using right now.
//!
//! The state's `active` pointer says who Pitstop last put in place, but the
//! tool can be signed in to someone else since (`/login`, `codex login`). The
//! login's own details decide: parking a token under the wrong account would
//! cost both accounts their login.

use crate::provider::{Details, ProviderId};
use crate::state::{AccountMeta, State};

/// What a tool is signed in to, as far as Pitstop can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Live {
    LoggedOut,
    /// The live login belongs to this enrolled account.
    Account(String),
    /// Signed in to someone Pitstop does not manage.
    Unmanaged {
        label: Option<String>,
        plan: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    Same,
    Different,
    /// Neither an identity nor an email on both sides to compare.
    Unknown,
}

/// Identities when both sides have one, otherwise emails.
fn compare(live: &Details, meta: &AccountMeta) -> Match {
    if let (Some(a), Some(b)) = (&live.identity, &meta.identity) {
        return if identities_match(a, b) {
            Match::Same
        } else {
            Match::Different
        };
    }
    match (&live.label, &meta.label) {
        (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => Match::Same,
        (Some(_), Some(_)) => Match::Different,
        _ => Match::Unknown,
    }
}

/// `<account>:<organization>` pairs agree where both say something: an empty
/// half is one side not knowing it yet, not a different value.
fn identities_match(a: &str, b: &str) -> bool {
    let (a_account, a_org) = a.split_once(':').unwrap_or((a, ""));
    let (b_account, b_org) = b.split_once(':').unwrap_or((b, ""));
    a_account == b_account && (a_org.is_empty() || b_org.is_empty() || a_org == b_org)
}

/// Whether `login` is certainly not `meta`'s account.
pub(crate) fn is_someone_else(login: &Details, meta: &AccountMeta) -> bool {
    compare(login, meta) == Match::Different
}

/// The enrolled account that is certainly this login, ignoring logins that
/// cannot be compared.
pub(crate) fn enrolled_as(state: &State, provider: ProviderId, login: &Details) -> Option<String> {
    state
        .accounts
        .iter()
        .find(|a| a.provider == provider && compare(login, a) == Match::Same)
        .map(|a| a.name.clone())
}

/// The enrolled account the live login belongs to, if any: the active one
/// unless the login is clearly someone else's, then any other that matches.
pub(crate) fn owner(state: &State, provider: ProviderId, live: &Details) -> Option<String> {
    let accounts = || state.accounts.iter().filter(|a| a.provider == provider);
    if let Some(active) = state.active_name(provider) {
        if let Some(meta) = accounts().find(|a| &a.name == active) {
            if compare(live, meta) != Match::Different {
                return Some(meta.name.clone());
            }
        }
    }
    accounts()
        .find(|a| compare(live, a) == Match::Same)
        .map(|a| a.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, label: Option<&str>, identity: Option<&str>) -> AccountMeta {
        AccountMeta {
            name: name.into(),
            provider: ProviderId::ClaudeCode,
            label: label.map(Into::into),
            parked_at: None,
            identity: identity.map(Into::into),
            plan: None,
        }
    }

    fn state(accounts: Vec<AccountMeta>, active: Option<&str>) -> State {
        let mut state = State {
            accounts,
            ..State::default()
        };
        if let Some(active) = active {
            state.active.insert("claude-code".into(), active.into());
        }
        state
    }

    fn login(label: Option<&str>, identity: Option<&str>) -> Details {
        Details {
            label: label.map(Into::into),
            identity: identity.map(Into::into),
            plan: None,
        }
    }

    fn owner_of(state: &State, live: &Details) -> Option<String> {
        owner(state, ProviderId::ClaudeCode, live)
    }

    fn work_and_personal(active: &str) -> State {
        state(
            vec![
                account("work", Some("a@x.com"), Some("acc-a:org-a")),
                account("personal", Some("b@x.com"), Some("acc-b:org-b")),
            ],
            Some(active),
        )
    }

    #[test]
    fn the_active_account_owns_its_own_login() {
        let state = work_and_personal("work");
        let live = login(Some("a@x.com"), Some("acc-a:org-a"));
        assert_eq!(owner_of(&state, &live).as_deref(), Some("work"));
    }

    #[test]
    fn a_login_to_another_enrolled_account_belongs_to_that_account() {
        let state = work_and_personal("work");
        let live = login(Some("b@x.com"), Some("acc-b:org-b"));
        assert_eq!(owner_of(&state, &live).as_deref(), Some("personal"));
    }

    #[test]
    fn a_login_nobody_enrolled_has_no_owner() {
        let state = work_and_personal("work");
        let live = login(Some("c@x.com"), Some("acc-c:org-c"));
        assert_eq!(owner_of(&state, &live), None);
    }

    #[test]
    fn the_same_email_in_another_organization_is_someone_else() {
        let state = work_and_personal("work");
        let live = login(Some("a@x.com"), Some("acc-a:org-other"));
        assert_eq!(owner_of(&state, &live), None);
    }

    #[test]
    fn an_organization_one_side_does_not_know_yet_still_matches() {
        let state = state(
            vec![account("work", Some("a@x.com"), Some("acc-a:"))],
            Some("work"),
        );
        let live = login(Some("a@x.com"), Some("acc-a:org-a"));
        assert_eq!(owner_of(&state, &live).as_deref(), Some("work"));
    }

    #[test]
    fn an_account_parked_by_0_1_0_is_matched_by_email() {
        let state = state(
            vec![
                account("work", Some("a@x.com"), None),
                account("personal", Some("b@x.com"), None),
            ],
            Some("work"),
        );
        let live = login(Some("B@X.com"), Some("acc-b:org-b"));
        assert_eq!(owner_of(&state, &live).as_deref(), Some("personal"));
    }

    #[test]
    fn a_login_that_cannot_be_compared_stays_with_the_active_account() {
        let state = state(vec![account("work", None, None)], Some("work"));
        assert_eq!(
            owner_of(&state, &login(None, None)).as_deref(),
            Some("work")
        );
    }

    #[test]
    fn with_no_active_account_an_unknown_login_has_no_owner() {
        let state = state(vec![account("work", None, None)], None);
        assert_eq!(owner_of(&state, &login(None, None)), None);
    }

    #[test]
    fn accounts_of_another_tool_never_own_the_login() {
        let mut codex = account("cx", Some("a@x.com"), Some("acc-a:org-a"));
        codex.provider = ProviderId::Codex;
        let state = state(vec![codex], None);
        let live = login(Some("a@x.com"), Some("acc-a:org-a"));
        assert_eq!(owner_of(&state, &live), None);
    }
}
