//! Enroll / switch / remove flows on top of Host + Provider + Vault + State.

use crate::error::{Error, Result};
use crate::host::Host;
use crate::live::{self, Live};
use crate::provider::{provider_for, Details, ProviderId};
use crate::state::{AccountMeta, State};
use crate::usage::{self, CodexToken, Token, UsageApi, UsageReport};
use crate::vault;

pub struct Engine<H: Host> {
    host: H,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SwitchOutcome {
    Switched {
        provider: ProviderId,
        name: String,
        /// The tool was signed in to a login no enrolled account owns; it was
        /// replaced, not parked.
        replaced: Option<UnmanagedLogin>,
    },
    AlreadyActive,
    /// `switch_keeping_unmanaged` stopped: the tool holds a login no enrolled
    /// account owns, which a switch would replace. Nothing was written.
    WouldReplace(UnmanagedLogin),
}

#[derive(Debug, PartialEq, Eq)]
pub struct SignedInAgain {
    /// The account was the one in use, so the new login went in use too.
    pub in_use: bool,
}

/// A login the tool held that Pitstop does not manage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmanagedLogin {
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub provider: ProviderId,
    pub name: String,
    pub report: UsageReport,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed,
    /// The entry was dropped from pitstop, but the tool's live credential was
    /// left untouched on purpose.
    RemovedActive,
}

pub fn vault_id(provider: ProviderId, name: &str) -> String {
    format!("{}--{}", provider.as_str(), name)
}

impl<H: Host> Engine<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn state(&self) -> Result<State> {
        State::load(&self.host)
    }

    /// Whose login `provider`'s tool is using right now.
    pub fn live(&self, provider: ProviderId) -> Result<Live> {
        let Some(current) = provider_for(provider).read_active(&self.host)? else {
            return Ok(Live::LoggedOut);
        };
        let state = State::load(&self.host)?;
        Ok(match live::owner(&state, provider, &current.details) {
            Some(name) => Live::Account(name),
            None => Live::Unmanaged {
                label: current.details.label,
                plan: current.details.plan,
            },
        })
    }

    pub fn enroll(&self, provider: ProviderId, name: &str) -> Result<AccountMeta> {
        validate_name(name)?;
        let mut state = State::load(&self.host)?;
        refuse_taken_name(&state, provider, name, None)?;
        let prov = provider_for(provider);
        let active = prov
            .read_active(&self.host)?
            .ok_or_else(|| Error::NotLoggedIn(provider.display_name().into()))?;
        if let Some(existing) = live::enrolled_as(&state, provider, &active.details) {
            return Err(Error::AlreadyEnrolled {
                name: existing,
                provider: provider.display_name().into(),
            });
        }
        vault::store(&self.host, &vault_id(provider, name), &active.blob)?;
        let meta = AccountMeta {
            name: name.into(),
            provider,
            label: active.details.label,
            parked_at: Some(now_epoch()),
            identity: active.details.identity,
            plan: active.details.plan,
        };
        state.accounts.push(meta.clone());
        state.active.insert(provider.as_str().into(), name.into());
        state.save(&self.host)?;
        Ok(meta)
    }

    pub fn switch(&self, name: &str, provider: Option<ProviderId>) -> Result<SwitchOutcome> {
        self.switch_inner(name, provider, true)
    }

    /// Like `switch`, but when the tool holds a login no enrolled account
    /// owns, answer `WouldReplace` and write nothing, so a caller can ask
    /// first. Decided on the same read of the login the switch would park.
    pub fn switch_keeping_unmanaged(
        &self,
        name: &str,
        provider: Option<ProviderId>,
    ) -> Result<SwitchOutcome> {
        self.switch_inner(name, provider, false)
    }

    fn switch_inner(
        &self,
        name: &str,
        provider: Option<ProviderId>,
        replace_unmanaged: bool,
    ) -> Result<SwitchOutcome> {
        let mut state = State::load(&self.host)?;
        let meta = resolve(&state, name, provider)?.clone();
        let pid = meta.provider;
        let prov = provider_for(pid);
        let current = prov.read_active(&self.host)?;
        let owner = current
            .as_ref()
            .and_then(|c| live::owner(&state, pid, &c.details));

        if owner.as_deref() == Some(meta.name.as_str()) {
            // Signed in to it already; only the record can be behind.
            if state.active_name(pid) != Some(&meta.name) {
                state.active.insert(pid.as_str().into(), meta.name.clone());
                state.save(&self.host)?;
            }
            return Ok(SwitchOutcome::AlreadyActive);
        }

        if let (Some(current), None, false) = (&current, &owner, replace_unmanaged) {
            return Ok(SwitchOutcome::WouldReplace(UnmanagedLogin {
                label: current.details.label.clone(),
            }));
        }

        // Fail before touching anything if the target cannot be produced.
        let target_blob = vault::load(&self.host, &vault_id(pid, &meta.name))?;

        // Park the live login under the account it belongs to. A login no
        // enrolled account owns is replaced, never filed under someone else.
        let mut replaced = None;
        if let Some(current) = &current {
            match &owner {
                Some(owner) => {
                    vault::store(&self.host, &vault_id(pid, owner), &current.blob)?;
                    if let Some(owner_meta) = state
                        .accounts
                        .iter_mut()
                        .find(|a| a.provider == pid && &a.name == owner)
                    {
                        owner_meta.parked_at = Some(now_epoch());
                        record_details(owner_meta, &current.details);
                    }
                }
                None => {
                    replaced = Some(UnmanagedLogin {
                        label: current.details.label.clone(),
                    })
                }
            }
        }

        if let Err(e) = prov.write_active(&self.host, &target_blob) {
            if let Some(current) = &current {
                let _ = prov.write_active(&self.host, &current.blob);
            }
            return Err(e);
        }

        state.active.insert(pid.as_str().into(), meta.name.clone());
        if let Err(e) = state.save(&self.host) {
            if let Some(current) = &current {
                let _ = prov.write_active(&self.host, &current.blob);
            }
            return Err(e);
        }
        Ok(SwitchOutcome::Switched {
            provider: pid,
            name: meta.name,
            replaced,
        })
    }

    /// Whether `name` could be enrolled for `provider`, checked before a
    /// sign-in starts so nobody signs in only to be told the name is taken.
    pub fn check_new_name(&self, provider: ProviderId, name: &str) -> Result<()> {
        validate_name(name)?;
        refuse_taken_name(&State::load(&self.host)?, provider, name, None)
    }

    /// Enroll a login a private sign-in produced (see `signin`) as a new
    /// account, not in use: the login the tool is using is not touched.
    pub fn add_signed_in(
        &self,
        provider: ProviderId,
        name: &str,
        blob: &[u8],
    ) -> Result<AccountMeta> {
        validate_name(name)?;
        let mut state = State::load(&self.host)?;
        refuse_taken_name(&state, provider, name, None)?;
        let details = provider_for(provider).details(blob);
        if let Some(existing) = live::enrolled_as(&state, provider, &details) {
            return Err(Error::AlreadyEnrolled {
                name: existing,
                provider: provider.display_name().into(),
            });
        }
        let id = vault_id(provider, name);
        vault::store(&self.host, &id, blob)?;
        let meta = AccountMeta {
            name: name.into(),
            provider,
            label: details.label,
            parked_at: Some(now_epoch()),
            identity: details.identity,
            plan: details.plan,
        };
        state.accounts.push(meta.clone());
        if let Err(e) = state.save(&self.host) {
            let _ = vault::delete(&self.host, &id);
            return Err(e);
        }
        Ok(meta)
    }

    /// Replace `name`'s parked login with a fresh sign-in of the same account;
    /// when the tool is using that account, the new login goes in use too.
    pub fn sign_in_again(
        &self,
        name: &str,
        provider: Option<ProviderId>,
        blob: &[u8],
    ) -> Result<SignedInAgain> {
        let mut state = State::load(&self.host)?;
        let meta = resolve(&state, name, provider)?.clone();
        let pid = meta.provider;
        let prov = provider_for(pid);
        let details = prov.details(blob);
        if live::is_someone_else(&details, &meta) {
            return Err(Error::WrongAccount {
                name: meta.name,
                label: details.label,
            });
        }

        let current = prov.read_active(&self.host)?;
        let in_use = current
            .as_ref()
            .and_then(|c| live::owner(&state, pid, &c.details))
            .as_deref()
            == Some(meta.name.as_str());
        vault::store(&self.host, &vault_id(pid, &meta.name), blob)?;
        if in_use {
            if let Err(e) = prov.write_active(&self.host, blob) {
                if let Some(current) = &current {
                    let _ = prov.write_active(&self.host, &current.blob);
                }
                return Err(e);
            }
            state.active.insert(pid.as_str().into(), meta.name.clone());
        }
        if let Some(account) = state
            .accounts
            .iter_mut()
            .find(|a| a.provider == pid && a.name == meta.name)
        {
            account.parked_at = Some(now_epoch());
            record_details(account, &details);
        }
        state.save(&self.host)?;
        Ok(SignedInAgain { in_use })
    }

    /// Give an account a new name, keeping its parked login and its place.
    pub fn rename(
        &self,
        name: &str,
        provider: Option<ProviderId>,
        new_name: &str,
    ) -> Result<AccountMeta> {
        validate_name(new_name)?;
        let mut state = State::load(&self.host)?;
        let meta = resolve(&state, name, provider)?.clone();
        let pid = meta.provider;
        if new_name == meta.name {
            return Ok(meta);
        }
        refuse_taken_name(&state, pid, new_name, Some(&meta.name))?;

        let (old_id, new_id) = (vault_id(pid, &meta.name), vault_id(pid, new_name));
        let blob = vault::load(&self.host, &old_id)?;
        vault::store(&self.host, &new_id, &blob)?;
        // Where only the case changes, both ids are one file on Windows and macOS.
        let one_file = old_id.eq_ignore_ascii_case(&new_id);

        let mut renamed = meta.clone();
        renamed.name = new_name.into();
        for account in &mut state.accounts {
            if account.provider == pid && account.name == meta.name {
                account.name = new_name.into();
            }
        }
        if state.active_name(pid) == Some(&meta.name) {
            state.active.insert(pid.as_str().into(), new_name.into());
        }
        if let Err(e) = state.save(&self.host) {
            if !one_file {
                let _ = vault::delete(&self.host, &new_id);
            }
            return Err(e);
        }
        if !one_file {
            // The rename has happened; a copy left under the old id is only clutter.
            let _ = vault::delete(&self.host, &old_id);
        }
        Ok(renamed)
    }

    pub fn remove(&self, name: &str, provider: Option<ProviderId>) -> Result<RemoveOutcome> {
        let mut state = State::load(&self.host)?;
        let meta = resolve(&state, name, provider)?.clone();
        let pid = meta.provider;
        vault::delete(&self.host, &vault_id(pid, &meta.name))?;
        state
            .accounts
            .retain(|a| !(a.provider == pid && a.name == meta.name));
        let was_active = state.active_name(pid).map(String::as_str) == Some(meta.name.as_str());
        if was_active {
            state.active.remove(pid.as_str());
        }
        state.save(&self.host)?;
        Ok(if was_active {
            RemoveOutcome::RemovedActive
        } else {
            RemoveOutcome::Removed
        })
    }

    /// Best-effort usage for every account of both tools. The account that
    /// owns a tool's live login is read with the live token, since its parked
    /// copy may be stale; every other account with its vault copy. Requests
    /// run in parallel.
    pub fn usage(&self, api: &dyn UsageApi) -> Result<Vec<AccountUsage>> {
        let state = State::load(&self.host)?;
        let now = now_epoch() as i64;

        // Each tool's live login and the account it belongs to. A login that
        // cannot be read, or that nobody enrolled owns, is read for nobody.
        let mut live_logins: Vec<(ProviderId, String, Vec<u8>)> = Vec::new();
        for pid in ProviderId::ALL {
            if !state.accounts.iter().any(|a| a.provider == pid) {
                continue;
            }
            if let Ok(Some(current)) = provider_for(pid).read_active(&self.host) {
                if let Some(owner) = live::owner(&state, pid, &current.details) {
                    live_logins.push((pid, owner, current.blob));
                }
            }
        }

        let requests: Vec<(&AccountMeta, std::result::Result<Request, UsageReport>)> = state
            .accounts
            .iter()
            .map(|a| {
                let live = live_logins
                    .iter()
                    .find(|(pid, owner, _)| *pid == a.provider && owner == &a.name);
                let blob = match live {
                    Some((_, _, blob)) => Ok(blob.clone()),
                    None => vault::load(&self.host, &vault_id(a.provider, &a.name))
                        .map_err(|e| UsageReport::Failed(e.to_string())),
                };
                (
                    a,
                    blob.and_then(|blob| Request::for_blob(a.provider, &blob, now)),
                )
            })
            .collect();

        Ok(std::thread::scope(|scope| {
            let handles: Vec<_> = requests
                .into_iter()
                .map(|(meta, request)| {
                    let handle = request.map(|request| scope.spawn(move || request.send(api)));
                    (meta, handle)
                })
                .collect();
            handles
                .into_iter()
                .map(|(meta, handle)| AccountUsage {
                    provider: meta.provider,
                    name: meta.name.clone(),
                    report: match handle {
                        Ok(h) => h
                            .join()
                            .unwrap_or_else(|_| UsageReport::Failed("request panicked".into())),
                        Err(report) => report,
                    },
                })
                .collect()
        }))
    }
}

/// One usage request, with the token it carries. Deliberately not `Debug`.
enum Request {
    Claude(String),
    Codex {
        access_token: String,
        account_id: String,
    },
}

impl Request {
    /// The request a parked or live credential allows, or why there is none:
    /// an expired access token is never sent, nor refreshed.
    fn for_blob(
        provider: ProviderId,
        blob: &[u8],
        now: i64,
    ) -> std::result::Result<Request, UsageReport> {
        let missing = || UsageReport::Failed("no access token in credential".into());
        match provider {
            ProviderId::ClaudeCode => match usage::claude_token(blob, now) {
                Token::Valid(token) => Ok(Request::Claude(token)),
                Token::Expired => Err(UsageReport::Expired),
                Token::Missing => Err(missing()),
            },
            ProviderId::Codex => match usage::codex_token(blob, now) {
                CodexToken::Valid {
                    access_token,
                    account_id,
                } => Ok(Request::Codex {
                    access_token,
                    account_id,
                }),
                CodexToken::Expired => Err(UsageReport::Expired),
                CodexToken::Missing => Err(missing()),
            },
        }
    }

    fn send(self, api: &dyn UsageApi) -> UsageReport {
        match self {
            Request::Claude(token) => usage::claude_report(api.claude(&token)),
            Request::Codex {
                access_token,
                account_id,
            } => usage::codex_report(api.codex(&access_token, &account_id)),
        }
    }
}

fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidName(name.into()))
    }
}

/// What a freshly parked login says about itself replaces what was recorded;
/// what it does not say keeps the recorded value.
fn record_details(meta: &mut AccountMeta, details: &Details) {
    if details.label.is_some() {
        meta.label.clone_from(&details.label);
    }
    if details.identity.is_some() {
        meta.identity.clone_from(&details.identity);
    }
    if details.plan.is_some() {
        meta.plan.clone_from(&details.plan);
    }
}

/// Vault files are named after accounts, and Windows and macOS file systems
/// ignore case: `Work` and `work` would share one parked credential.
fn refuse_taken_name(
    state: &State,
    provider: ProviderId,
    name: &str,
    except: Option<&str>,
) -> Result<()> {
    if state.accounts.iter().any(|a| {
        a.provider == provider
            && Some(a.name.as_str()) != except
            && a.name.eq_ignore_ascii_case(name)
    }) {
        return Err(Error::DuplicateAccount(
            name.into(),
            provider.display_name().into(),
        ));
    }
    Ok(())
}

fn resolve<'a>(
    state: &'a State,
    name: &str,
    provider: Option<ProviderId>,
) -> Result<&'a AccountMeta> {
    let mut matches = state
        .accounts
        .iter()
        .filter(|a| a.name == name && provider.is_none_or(|p| a.provider == p));
    match (matches.next(), matches.next()) {
        (Some(meta), None) => Ok(meta),
        (Some(_), Some(_)) => Err(Error::AmbiguousAccount(name.into())),
        (None, _) => Err(Error::UnknownAccount(name.into())),
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::claude_code::ClaudeCode;
    use crate::state::state_path;
    use crate::testing::FakeHost;

    const TOKEN_A: &str = "SECRET-TOKEN-AAA";
    const TOKEN_B: &str = "SECRET-TOKEN-BBB";

    fn creds(token: &str) -> String {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"R"}}}}"#)
    }

    /// `~/.claude.json` as Claude Code writes it after a sign-in: one account
    /// per email here, in one organization.
    fn config(email: &str) -> String {
        format!(
            r#"{{"numStartups":7,"oauthAccount":{{"emailAddress":"{email}","accountUuid":"uuid-{email}","organizationUuid":"org-1"}}}}"#
        )
    }

    /// A FakeHost where Claude Code account A (TOKEN_A) is logged in.
    fn host_logged_in(token: &str, email: &str) -> FakeHost {
        let host = FakeHost::new();
        sign_in_as(&host, token, email);
        host
    }

    /// What `/login` does: a new token and that account's `oauthAccount`.
    fn sign_in_as(host: &FakeHost, token: &str, email: &str) {
        host.put_file(ClaudeCode::credentials_path(host), creds(token));
        host.put_file(ClaudeCode::config_path(host), config(email));
    }

    fn active_token(host: &FakeHost) -> String {
        let bytes = host.file(&ClaudeCode::credentials_path(host)).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn enroll_parks_active_and_marks_it_active() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        let meta = engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        assert_eq!(meta.label.as_deref(), Some("a@x.com"));
        let state = engine.state().unwrap();
        assert_eq!(state.accounts.len(), 1);
        assert_eq!(state.active_name(ProviderId::ClaudeCode).unwrap(), "work");
    }

    #[test]
    fn live_says_whose_login_the_tool_is_using() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Unmanaged {
                label: Some("a@x.com".into()),
                plan: None
            }
        );

        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("work".into())
        );

        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Unmanaged {
                label: Some("b@x.com".into()),
                plan: None
            }
        );
    }

    #[test]
    fn live_is_logged_out_without_a_credential() {
        let engine = Engine::new(FakeHost::new());
        assert_eq!(engine.live(ProviderId::Codex).unwrap(), Live::LoggedOut);
    }

    #[test]
    fn enroll_rejects_a_name_that_differs_only_in_case() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        assert!(matches!(
            engine.enroll(ProviderId::ClaudeCode, "Work"),
            Err(Error::DuplicateAccount(..))
        ));
    }

    #[test]
    fn enroll_refuses_a_login_already_enrolled_under_another_name() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        let err = engine.enroll(ProviderId::ClaudeCode, "again").unwrap_err();
        assert!(
            matches!(&err, Error::AlreadyEnrolled { name, .. } if name == "work"),
            "{err:?}"
        );
        assert_eq!(engine.state().unwrap().accounts.len(), 1);
    }

    #[test]
    fn enroll_records_who_the_login_belongs_to_and_its_plan() {
        let host = host_logged_in(TOKEN_A, "a@x.com");
        host.put_file(
            ClaudeCode::credentials_path(&host),
            r#"{"claudeAiOauth":{"accessToken":"SECRET","subscriptionType":"max","rateLimitTier":"default_claude_max_5x"}}"#,
        );
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        let state = engine.state().unwrap();
        assert_eq!(
            state.accounts[0].identity.as_deref(),
            Some("uuid-a@x.com:org-1")
        );
        assert_eq!(state.accounts[0].plan.as_deref(), Some("Max 5x"));
    }

    #[test]
    fn enroll_fails_when_not_logged_in_and_leaves_no_state() {
        let engine = Engine::new(FakeHost::new());
        let err = engine.enroll(ProviderId::ClaudeCode, "work").unwrap_err();
        assert!(matches!(err, Error::NotLoggedIn(_)));
        assert!(engine.host().file(&state_path(engine.host())).is_none());
    }

    #[test]
    fn enroll_rejects_duplicate_name_without_overwriting_parked_blob() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        // different credential now live
        engine
            .host()
            .put_file(ClaudeCode::credentials_path(engine.host()), creds(TOKEN_B));
        let err = engine.enroll(ProviderId::ClaudeCode, "work").unwrap_err();
        assert!(matches!(err, Error::DuplicateAccount(..)));
        // parked blob still decrypts to TOKEN_A's blob (switch back proves it)
        engine.switch("work", None).unwrap(); // no-op switch: already active
        assert!(
            active_token(engine.host()).contains(TOKEN_B),
            "active file untouched"
        );
    }

    #[test]
    fn enroll_rejects_invalid_names() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        for bad in ["", "has space", "slash/name", &"x".repeat(65)] {
            assert!(matches!(
                engine.enroll(ProviderId::ClaudeCode, bad),
                Err(Error::InvalidName(_))
            ));
        }
    }

    #[test]
    fn switch_swaps_credentials_oauth_account_and_state() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        // user signs into account B with the tool itself
        engine
            .host()
            .put_file(ClaudeCode::credentials_path(engine.host()), creds(TOKEN_B));
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("b@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();

        let outcome = engine.switch("work", None).unwrap();
        assert_eq!(
            outcome,
            SwitchOutcome::Switched {
                provider: ProviderId::ClaudeCode,
                name: "work".into(),
                replaced: None,
            }
        );
        assert!(active_token(engine.host()).contains(TOKEN_A));
        let cfg: serde_json::Value = serde_json::from_slice(
            &engine
                .host()
                .file(&ClaudeCode::config_path(engine.host()))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cfg["oauthAccount"]["emailAddress"], "a@x.com");
        assert_eq!(cfg["numStartups"], 7, "rest of .claude.json preserved");
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "work"
        );

        // and back: B's token was parked during the switch, not lost
        engine.switch("personal", None).unwrap();
        assert!(active_token(engine.host()).contains(TOKEN_B));
    }

    #[test]
    fn switch_to_active_account_is_a_noop() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        let before = active_token(engine.host());
        assert_eq!(
            engine.switch("work", None).unwrap(),
            SwitchOutcome::AlreadyActive
        );
        assert_eq!(active_token(engine.host()), before);
    }

    #[test]
    fn switch_parks_the_latest_live_token_for_the_recorded_owner() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        // personal's token gets refreshed by the tool after enrollment
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            creds("SECRET-TOKEN-B2"),
        );
        engine.switch("work", None).unwrap();
        engine.switch("personal", None).unwrap();
        assert!(active_token(engine.host()).contains("SECRET-TOKEN-B2"));
    }

    fn parked(engine: &Engine<FakeHost>, name: &str) -> String {
        let blob = vault::load(engine.host(), &vault_id(ProviderId::ClaudeCode, name)).unwrap();
        String::from_utf8(blob).unwrap()
    }

    /// `work` (TOKEN_A) and `personal` (TOKEN_B) enrolled, `work` in use.
    fn work_and_personal() -> Engine<FakeHost> {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        engine.switch("work", None).unwrap();
        engine
    }

    #[test]
    fn switch_parks_a_login_changed_outside_pitstop_under_its_own_account() {
        let engine = work_and_personal();
        // personal signed in with the tool itself; the state still says work
        sign_in_as(engine.host(), "SECRET-TOKEN-B-FRESH", "b@x.com");
        engine.switch("work", None).unwrap();
        assert!(active_token(engine.host()).contains(TOKEN_A));
        assert!(parked(&engine, "personal").contains("SECRET-TOKEN-B-FRESH"));
        assert!(parked(&engine, "work").contains(TOKEN_A));
    }

    #[test]
    fn switch_never_parks_an_unmanaged_login_in_another_accounts_place() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), "SECRET-TOKEN-CCC", "c@x.com");
        let outcome = engine.switch("personal", None).unwrap();
        assert_eq!(
            outcome,
            SwitchOutcome::Switched {
                provider: ProviderId::ClaudeCode,
                name: "personal".into(),
                replaced: Some(UnmanagedLogin {
                    label: Some("c@x.com".into())
                }),
            }
        );
        assert!(active_token(engine.host()).contains(TOKEN_B));
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert!(!parked(&engine, "work").contains("SECRET-TOKEN-CCC"));
    }

    #[test]
    fn a_careful_switch_asks_before_replacing_an_unmanaged_login() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), "SECRET-TOKEN-CCC", "c@x.com");
        assert_eq!(
            engine.switch_keeping_unmanaged("personal", None).unwrap(),
            SwitchOutcome::WouldReplace(UnmanagedLogin {
                label: Some("c@x.com".into())
            })
        );
        assert!(active_token(engine.host()).contains("SECRET-TOKEN-CCC"));
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "work"
        );
    }

    #[test]
    fn a_careful_switch_from_a_managed_login_switches() {
        let engine = work_and_personal();
        assert!(matches!(
            engine.switch_keeping_unmanaged("personal", None).unwrap(),
            SwitchOutcome::Switched { replaced: None, .. }
        ));
        assert!(active_token(engine.host()).contains(TOKEN_B));
    }

    #[test]
    fn switch_signs_a_logged_out_tool_back_in_to_the_recorded_account() {
        let engine = work_and_personal();
        engine
            .host()
            .remove_file(&ClaudeCode::credentials_path(engine.host()))
            .unwrap();
        assert_eq!(
            engine.switch("work", None).unwrap(),
            SwitchOutcome::Switched {
                provider: ProviderId::ClaudeCode,
                name: "work".into(),
                replaced: None,
            }
        );
        assert!(active_token(engine.host()).contains(TOKEN_A));
    }

    #[test]
    fn switch_to_the_account_already_signed_in_only_corrects_the_record() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        assert_eq!(
            engine.switch("personal", None).unwrap(),
            SwitchOutcome::AlreadyActive
        );
        assert!(active_token(engine.host()).contains(TOKEN_B));
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "personal"
        );
    }

    #[test]
    fn switching_away_records_the_plan_the_login_says_now() {
        let engine = work_and_personal();
        // work was upgraded since it was enrolled
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            r#"{"claudeAiOauth":{"accessToken":"SECRET-TOKEN-AAA2","subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}"#,
        );
        engine.switch("personal", None).unwrap();
        let state = engine.state().unwrap();
        let work = state.accounts.iter().find(|a| a.name == "work").unwrap();
        assert_eq!(work.plan.as_deref(), Some("Max 20x"));
    }

    #[test]
    fn rename_moves_the_parked_login_to_the_new_name() {
        let engine = work_and_personal();
        engine.rename("personal", None, "spare").unwrap();
        let state = engine.state().unwrap();
        let names: Vec<_> = state.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work", "spare"]);
        assert!(parked(&engine, "spare").contains(TOKEN_B));
        assert!(matches!(
            vault::load(engine.host(), &vault_id(ProviderId::ClaudeCode, "personal")),
            Err(Error::VaultBlobMissing(_))
        ));
    }

    #[test]
    fn rename_keeps_the_account_in_use_in_use() {
        let engine = work_and_personal();
        engine.rename("work", None, "job").unwrap();
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "job"
        );
        assert!(active_token(engine.host()).contains(TOKEN_A));
        engine.switch("personal", None).unwrap();
        engine.switch("job", None).unwrap();
        assert!(active_token(engine.host()).contains(TOKEN_A));
    }

    #[test]
    fn renaming_only_the_case_keeps_the_parked_login_where_case_is_ignored() {
        let engine = Engine::new(FakeHost::case_insensitive());
        sign_in_as(engine.host(), TOKEN_A, "a@x.com");
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        engine.rename("work", None, "Work").unwrap();
        engine.switch("Work", None).unwrap();
        assert!(active_token(engine.host()).contains(TOKEN_A));
    }

    #[test]
    fn rename_refuses_a_name_taken_in_any_case() {
        let engine = work_and_personal();
        assert!(matches!(
            engine.rename("work", None, "PERSONAL"),
            Err(Error::DuplicateAccount(..))
        ));
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert_eq!(engine.state().unwrap().accounts.len(), 2);
    }

    #[test]
    fn rename_refuses_an_invalid_name() {
        let engine = work_and_personal();
        assert!(matches!(
            engine.rename("work", None, "has space"),
            Err(Error::InvalidName(_))
        ));
    }

    /// A login as a private sign-in hands it over: the vault blob format.
    fn claude_blob(token: &str, email: &str) -> Vec<u8> {
        let config: serde_json::Value = serde_json::from_str(&config(email)).unwrap();
        serde_json::to_vec(&serde_json::json!({
            "credentials": creds(token),
            "oauth_account": config["oauthAccount"],
        }))
        .unwrap()
    }

    #[test]
    fn a_new_name_is_checked_before_any_sign_in_starts() {
        let engine = work_and_personal();
        assert!(matches!(
            engine.check_new_name(ProviderId::ClaudeCode, "Work"),
            Err(Error::DuplicateAccount(..))
        ));
        assert!(matches!(
            engine.check_new_name(ProviderId::ClaudeCode, "has space"),
            Err(Error::InvalidName(_))
        ));
        assert!(engine
            .check_new_name(ProviderId::ClaudeCode, "spare")
            .is_ok());
        // names belong to their tool
        assert!(engine.check_new_name(ProviderId::Codex, "work").is_ok());
    }

    #[test]
    fn add_signed_in_parks_a_new_account_without_touching_the_login_in_use() {
        let engine = work_and_personal();
        let meta = engine
            .add_signed_in(
                ProviderId::ClaudeCode,
                "spare",
                &claude_blob("SECRET-TOKEN-NEW", "n@x.com"),
            )
            .unwrap();
        assert_eq!(meta.label.as_deref(), Some("n@x.com"));
        assert_eq!(meta.identity.as_deref(), Some("uuid-n@x.com:org-1"));
        assert!(parked(&engine, "spare").contains("SECRET-TOKEN-NEW"));
        assert!(active_token(engine.host()).contains(TOKEN_A));
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "work"
        );
    }

    #[test]
    fn add_signed_in_refuses_a_login_already_enrolled() {
        let engine = work_and_personal();
        let err = engine
            .add_signed_in(
                ProviderId::ClaudeCode,
                "spare",
                &claude_blob("SECRET-X", "b@x.com"),
            )
            .unwrap_err();
        assert!(
            matches!(&err, Error::AlreadyEnrolled { name, .. } if name == "personal"),
            "{err:?}"
        );
        assert_eq!(engine.state().unwrap().accounts.len(), 2);
    }

    #[test]
    fn add_signed_in_refuses_a_taken_name() {
        let engine = work_and_personal();
        assert!(matches!(
            engine.add_signed_in(
                ProviderId::ClaudeCode,
                "Work",
                &claude_blob("SECRET-X", "n@x.com")
            ),
            Err(Error::DuplicateAccount(..))
        ));
        assert!(parked(&engine, "work").contains(TOKEN_A));
    }

    #[test]
    fn signing_in_again_replaces_a_parked_login() {
        let engine = work_and_personal();
        let again = engine
            .sign_in_again("personal", None, &claude_blob("SECRET-B-NEW", "b@x.com"))
            .unwrap();
        assert!(!again.in_use);
        assert!(parked(&engine, "personal").contains("SECRET-B-NEW"));
        assert!(active_token(engine.host()).contains(TOKEN_A));
    }

    #[test]
    fn signing_in_again_to_the_account_in_use_puts_the_new_login_in_use() {
        let engine = work_and_personal();
        let again = engine
            .sign_in_again("work", None, &claude_blob("SECRET-A-NEW", "a@x.com"))
            .unwrap();
        assert!(again.in_use);
        assert!(active_token(engine.host()).contains("SECRET-A-NEW"));
        assert!(parked(&engine, "work").contains("SECRET-A-NEW"));
    }

    #[test]
    fn signing_in_again_as_someone_else_is_refused() {
        let engine = work_and_personal();
        let err = engine
            .sign_in_again("personal", None, &claude_blob("SECRET-X", "x@x.com"))
            .unwrap_err();
        assert!(matches!(err, Error::WrongAccount { .. }), "{err:?}");
        assert!(parked(&engine, "personal").contains(TOKEN_B));
    }

    #[test]
    fn switch_unknown_name_fails() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        assert!(matches!(
            engine.switch("nope", None),
            Err(Error::UnknownAccount(_))
        ));
    }

    #[test]
    fn switch_rolls_back_when_a_mid_write_fails() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        engine
            .host()
            .put_file(ClaudeCode::credentials_path(engine.host()), creds(TOKEN_B));
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("b@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();

        // the second file written during write_active fails once
        engine
            .host()
            .fail_next_writes(ClaudeCode::config_path(engine.host()), 1);
        let err = engine.switch("work", None).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));

        // live credential restored to B, state still says personal
        assert!(active_token(engine.host()).contains(TOKEN_B));
        let cfg: serde_json::Value = serde_json::from_slice(
            &engine
                .host()
                .file(&ClaudeCode::config_path(engine.host()))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cfg["oauthAccount"]["emailAddress"], "b@x.com");
        assert_eq!(
            engine
                .state()
                .unwrap()
                .active_name(ProviderId::ClaudeCode)
                .unwrap(),
            "personal"
        );
    }

    #[test]
    fn remove_parked_account_deletes_blob_and_entry_only() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();

        assert_eq!(engine.remove("work", None).unwrap(), RemoveOutcome::Removed);
        let state = engine.state().unwrap();
        assert_eq!(state.accounts.len(), 1);
        assert!(
            active_token(engine.host()).contains(TOKEN_B),
            "live credential untouched"
        );
        assert!(matches!(
            engine.switch("work", None),
            Err(Error::UnknownAccount(_))
        ));
    }

    #[test]
    fn remove_active_account_warns_and_keeps_live_credential() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        assert_eq!(
            engine.remove("work", None).unwrap(),
            RemoveOutcome::RemovedActive
        );
        assert!(active_token(engine.host()).contains(TOKEN_A));
        assert!(engine.state().unwrap().accounts.is_empty());
    }

    #[test]
    fn secrets_never_appear_in_state_file() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        engine.switch("work", None).unwrap();

        let state_bytes = engine.host().file(&state_path(engine.host())).unwrap();
        let state_text = String::from_utf8(state_bytes).unwrap();
        assert!(!state_text.contains(TOKEN_A));
        assert!(!state_text.contains(TOKEN_B));
    }

    struct FakeApi {
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl FakeApi {
        fn new() -> Self {
            Self {
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl UsageApi for FakeApi {
        fn claude(&self, token: &str) -> std::result::Result<serde_json::Value, usage::FetchError> {
            self.seen.lock().unwrap().push(token.to_owned());
            match token {
                TOKEN_A => Ok(serde_json::json!({"five_hour": {"utilization": 10}})),
                "SECRET-TOKEN-B2" => Ok(serde_json::json!({"five_hour": {"utilization": 20}})),
                _ => Err(usage::FetchError::Unauthorized),
            }
        }

        fn codex(
            &self,
            token: &str,
            account_id: &str,
        ) -> std::result::Result<serde_json::Value, usage::FetchError> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{token}|{account_id}"));
            Ok(serde_json::json!({"rate_limit": {
                "primary_window": {"used_percent": 7, "limit_window_seconds": 18_000},
                "secondary_window": null
            }}))
        }
    }

    fn report_for<'a>(all: &'a [AccountUsage], name: &str) -> &'a UsageReport {
        &all.iter().find(|u| u.name == name).unwrap().report
    }

    fn five_hour(report: &UsageReport) -> f64 {
        match report {
            UsageReport::Ok(u) => u.windows[0].percent,
            other => panic!("expected usage, got {other:?}"),
        }
    }

    #[test]
    fn usage_reads_active_with_live_token_and_parked_with_vault_token() {
        let engine = Engine::new(host_logged_in(TOKEN_A, "a@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        // Claude Code refreshes the live token after enrollment; the vault copy is stale
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            creds("SECRET-TOKEN-B2"),
        );

        let api = FakeApi::new();
        let all = engine.usage(&api).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
        assert_eq!(five_hour(report_for(&all, "personal")), 20.0);
        let mut seen = api.seen.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, ["SECRET-TOKEN-AAA", "SECRET-TOKEN-B2"]);
    }

    #[test]
    fn usage_skips_expired_tokens_and_maps_refusals() {
        let expired = r#"{"claudeAiOauth":{"accessToken":"SECRET-OLD","expiresAt":1000}}"#;
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), expired);
        host.put_file(ClaudeCode::config_path(&host), config("a@x.com"));
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "old").unwrap();
        sign_in_as(engine.host(), "SECRET-REVOKED", "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "revoked").unwrap();

        let api = FakeApi::new();
        let all = engine.usage(&api).unwrap();
        assert_eq!(report_for(&all, "old"), &UsageReport::Expired);
        assert_eq!(report_for(&all, "revoked"), &UsageReport::Unauthorized);
        assert_eq!(api.seen.lock().unwrap().as_slice(), ["SECRET-REVOKED"]);
    }

    #[test]
    fn usage_reads_codex_accounts_with_their_token_and_account() {
        let b64 =
            |s: &str| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s);
        let id_token = format!("h.{}.s", b64(r#"{"email":"c@x.com"}"#));
        let access_token = format!("h.{}.s", b64(r#"{"exp":4102444800}"#));
        let host = FakeHost::new();
        host.put_file(
            host.home().join(".codex").join("auth.json"),
            format!(
                r#"{{"tokens":{{"id_token":"{id_token}","access_token":"{access_token}","account_id":"ws-1"}}}}"#
            ),
        );
        let engine = Engine::new(host);
        engine.enroll(ProviderId::Codex, "cx").unwrap();

        let api = FakeApi::new();
        let all = engine.usage(&api).unwrap();
        assert_eq!(five_hour(report_for(&all, "cx")), 7.0);
        assert_eq!(
            api.seen.lock().unwrap().as_slice(),
            [format!("{access_token}|ws-1")]
        );
    }

    #[test]
    fn usage_reads_the_live_token_for_the_account_that_owns_the_login() {
        let engine = work_and_personal();
        // personal signed in with the tool itself; the state still says work
        sign_in_as(engine.host(), "SECRET-TOKEN-B2", "b@x.com");
        let all = engine.usage(&FakeApi::new()).unwrap();
        assert_eq!(five_hour(report_for(&all, "personal")), 20.0);
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
    }
}
