//! Whose login a tool is using right now.
//!
//! The state's `active` pointer says who Pitstop last put in place, but the
//! tool can be signed in to someone else since (`/login`, `codex login`). The
//! login's own details decide: parking a token under the wrong account would
//! cost both accounts their login.
//!
//! Claude Code keeps its token (the Keychain, or `.credentials.json`) apart
//! from the record of whose it is (`~/.claude.json#oauthAccount`), and the two
//! can part ways: a sign-in that rewrote one and not the other leaves the
//! record naming an account whose login is gone. The token says nothing of
//! its own, so Anthropic is asked whose it is, once per token ([`Checked`]),
//! and that answer wins over the record.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::host::Host;
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
        /// The account the login's own record names, where its token was
        /// found to be someone else's.
        recorded: Option<String>,
    },
}

/// Whose the service said a tool's live token was. Only a fingerprint of the
/// token is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    /// The first 16 hex digits of the token's SHA-256.
    pub token: String,
    /// `<account>:<organization>`, as accounts are recorded.
    pub identity: String,
    pub label: Option<String>,
    /// The identity the login's record named instead, where the check found
    /// it naming someone else. Claude Code renewing the token changes
    /// neither, so a renewed token under that same record is still the
    /// checked account's until it is asked about.
    #[serde(default)]
    pub recorded: Option<String>,
}

pub fn checked_path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("checked.json")
}

fn all_checked(host: &dyn Host) -> BTreeMap<String, Checked> {
    host.read(&checked_path(host))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The last check of `provider`'s live token, if there was one.
pub(crate) fn checked(host: &dyn Host, provider: ProviderId) -> Option<Checked> {
    all_checked(host).remove(provider.as_str())
}

pub(crate) fn record_check(host: &dyn Host, provider: ProviderId, check: Checked) -> Result<()> {
    let mut all = all_checked(host);
    all.insert(provider.as_str().into(), check);
    save_checked(host, &all)
}

/// Forget `provider`'s check: the login it was of has been replaced.
pub(crate) fn forget_check(host: &dyn Host, provider: ProviderId) -> Result<()> {
    let mut all = all_checked(host);
    if all.remove(provider.as_str()).is_none() {
        return Ok(());
    }
    save_checked(host, &all)
}

fn save_checked(host: &dyn Host, all: &BTreeMap<String, Checked>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(all)
        .map_err(|e| Error::InvalidData(format!("checked.json: {e}")))?;
    host.write_atomic(&checked_path(host), &bytes)
}

pub(crate) fn fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};

    Sha256::digest(token.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A login as the last check found it: the checked account's where that
/// check was of this token, or of the token this one renewed (the record
/// that named someone else unchanged). Where the record names someone else,
/// it comes back beside it; a record naming nobody just takes the check's.
pub(crate) fn as_checked(
    login: Details,
    token: Option<&str>,
    check: Option<&Checked>,
) -> (Details, Option<Details>) {
    let Some(check) = check.filter(|check| check_applies(&login, token, check)) else {
        return (login, None);
    };
    let checked = Details {
        label: check.label.clone(),
        identity: Some(check.identity.clone()),
        // The plan is read from the token's own credentials.
        plan: login.plan.clone(),
    };
    match compare_logins(&login, &checked) {
        Match::Same => (login, None),
        Match::Different => (checked, Some(login)),
        Match::Unknown => (checked, None),
    }
}

/// Whether `check` says whose `login` is: it was of this token, or of the
/// token this one renewed (the record that named someone else unchanged).
/// Otherwise only the login's record says whose it is.
fn check_applies(login: &Details, token: Option<&str>, check: &Checked) -> bool {
    let this_token = token.is_some_and(|t| fingerprint(t) == check.token);
    this_token || (check.recorded.is_some() && check.recorded == login.identity)
}

/// Where the check of `name`'s parked login is kept, beside the live login's.
fn parked_key(provider: ProviderId, name: &str) -> String {
    format!("{}/{name}", provider.as_str())
}

/// The last check of whose `name`'s parked token is, if there was one.
pub(crate) fn parked_check(host: &dyn Host, provider: ProviderId, name: &str) -> Option<Checked> {
    all_checked(host).remove(&parked_key(provider, name))
}

/// Keep the `found` checks of parked logins, by account name, and drop any
/// kept for an account of `provider` no longer among `enrolled`. Written
/// only when something changed.
pub(crate) fn record_parked_checks(
    host: &dyn Host,
    provider: ProviderId,
    found: Vec<(String, Checked)>,
    enrolled: &[&str],
) -> Result<()> {
    let mut all = all_checked(host);
    let before = all.len();
    let prefix = parked_key(provider, "");
    all.retain(|key, _| {
        key.strip_prefix(&prefix)
            .is_none_or(|name| enrolled.contains(&name))
    });
    if found.is_empty() && all.len() == before {
        return Ok(());
    }
    for (name, check) in found {
        all.insert(parked_key(provider, &name), check);
    }
    save_checked(host, &all)
}

/// Whether a check found `login`'s record naming someone else, and the
/// identity it named if so: what [`Checked::recorded`] keeps.
pub(crate) fn misrecorded_as(
    login: &Details,
    identity: &str,
    label: Option<&str>,
) -> Option<String> {
    let checked = Details {
        label: label.map(str::to_owned),
        identity: Some(identity.to_owned()),
        plan: None,
    };
    (compare_logins(login, &checked) == Match::Different)
        .then(|| login.identity.clone())
        .flatten()
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
    compare_parts(
        (live.identity.as_deref(), live.label.as_deref()),
        (meta.identity.as_deref(), meta.label.as_deref()),
    )
}

fn compare_logins(a: &Details, b: &Details) -> Match {
    compare_parts(
        (a.identity.as_deref(), a.label.as_deref()),
        (b.identity.as_deref(), b.label.as_deref()),
    )
}

/// `(identity, email)` pairs: identities when both have one, otherwise emails.
fn compare_parts(a: (Option<&str>, Option<&str>), b: (Option<&str>, Option<&str>)) -> Match {
    if let (Some(a), Some(b)) = (a.0, b.0) {
        return if identities_match(a, b) {
            Match::Same
        } else {
            Match::Different
        };
    }
    match (a.1, b.1) {
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

    fn check_of(token: &str, label: &str, identity: &str) -> Checked {
        Checked {
            token: fingerprint(token),
            identity: identity.into(),
            label: Some(label.into()),
            recorded: None,
        }
    }

    #[test]
    fn a_checked_token_belongs_to_whoever_the_check_found() {
        let record = login(Some("a@x.com"), Some("acc-a:org-a"));
        let check = check_of("TOKEN", "c@x.com", "acc-c:org-a");
        let (found, recorded) = as_checked(record.clone(), Some("TOKEN"), Some(&check));
        assert_eq!(found.label.as_deref(), Some("c@x.com"));
        assert_eq!(found.identity.as_deref(), Some("acc-c:org-a"));
        assert_eq!(recorded, Some(record.clone()));
        // so the account the record names doesn't own it
        let state = work_and_personal("work");
        assert_eq!(owner_of(&state, &found), None);

        // a check of another token says nothing of this one
        let (kept, recorded) = as_checked(record.clone(), Some("NEWER"), Some(&check));
        assert_eq!((kept, recorded), (record.clone(), None));
        // nor does a check that agrees with the record
        let agrees = check_of("TOKEN", "a@x.com", "acc-a:org-a");
        let (kept, recorded) = as_checked(record.clone(), Some("TOKEN"), Some(&agrees));
        assert_eq!((kept, recorded), (record, None));
    }

    #[test]
    fn a_renewed_token_under_the_same_wrong_record_stays_the_checked_accounts() {
        let record = login(Some("a@x.com"), Some("acc-a:org-a"));
        let check = Checked {
            recorded: misrecorded_as(&record, "acc-c:org-a", Some("c@x.com")),
            ..check_of("TOKEN", "c@x.com", "acc-c:org-a")
        };
        assert_eq!(check.recorded.as_deref(), Some("acc-a:org-a"));
        let (found, recorded) = as_checked(record.clone(), Some("RENEWED"), Some(&check));
        assert_eq!(found.label.as_deref(), Some("c@x.com"));
        assert_eq!(recorded, Some(record));
        // a record naming someone else again is a new login, not a renewal
        let other = login(Some("b@x.com"), Some("acc-b:org-b"));
        let (kept, recorded) = as_checked(other.clone(), Some("RENEWED"), Some(&check));
        assert_eq!((kept, recorded), (other, None));
        // and a check that found nothing amiss carries nothing over
        let fine = check_of("TOKEN", "a@x.com", "acc-a:org-a");
        let record = login(Some("a@x.com"), Some("acc-a:org-a"));
        assert_eq!(
            misrecorded_as(&record, &fine.identity, fine.label.as_deref()),
            None
        );
    }

    #[test]
    fn a_record_naming_nobody_takes_the_checks_account_without_a_mismatch() {
        let check = check_of("TOKEN", "c@x.com", "acc-c:org-a");
        let (found, recorded) = as_checked(login(None, None), Some("TOKEN"), Some(&check));
        assert_eq!(found.identity.as_deref(), Some("acc-c:org-a"));
        assert_eq!(recorded, None);
        // nor is an email-only record that agrees a mismatch
        let (_, recorded) = as_checked(login(Some("C@x.com"), None), Some("TOKEN"), Some(&check));
        assert_eq!(recorded, None);
    }

    #[test]
    fn fingerprints_are_short_and_keep_nothing_of_the_token() {
        let print = fingerprint("SECRET-TOKEN");
        assert_eq!(print.len(), 16);
        assert!(!print.contains("SECRET"));
        assert_eq!(print, fingerprint("SECRET-TOKEN"));
        assert_ne!(print, fingerprint("SECRET-TOKEM"));
    }
}
