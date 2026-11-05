//! Enroll / switch / remove flows on top of Host + Provider + Vault + State.

use crate::error::{Error, Result};
use crate::host::Host;
use crate::live::{self, Live};
use crate::provider::{claude_code, provider_for, ActiveCredential, Details, ProviderId};
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

/// What renewing one parked login came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renewal {
    Renewed,
    /// The login can't be renewed: Anthropic no longer takes its refresh
    /// token, or it holds none. Only signing in again mends it.
    Refused,
    Failed(String),
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
        let Some(LiveLogin {
            current,
            misrecorded,
            ..
        }) = self.read_live(provider)?
        else {
            return Ok(Live::LoggedOut);
        };
        let state = State::load(&self.host)?;
        Ok(match live::owner(&state, provider, &current.details) {
            Some(name) => Live::Account(name),
            None => Live::Unmanaged {
                label: current.details.label,
                plan: current.details.plan,
                recorded: misrecorded.and_then(|record| record.label),
            },
        })
    }

    /// The tool's live login, its owner as the last check of its token
    /// found it (see [`live::Checked`]).
    fn read_live(&self, provider: ProviderId) -> Result<Option<LiveLogin>> {
        let Some(mut current) = provider_for(provider).read_active(&self.host)? else {
            return Ok(None);
        };
        let record = current.details.clone();
        let mut misrecorded = None;
        let mut verified = true;
        if provider == ProviderId::ClaudeCode {
            let token = usage::claude_access_token(&current.blob);
            let check = live::checked(&self.host, provider);
            // Only a check of this very token: one of the token it renewed
            // may be of a login another sign-in has replaced since.
            verified = check.as_ref().is_some_and(|c| {
                token
                    .as_deref()
                    .is_some_and(|t| live::fingerprint(t) == c.token)
            });
            let (details, record) = live::as_checked(
                std::mem::take(&mut current.details),
                token.as_deref(),
                check.as_ref(),
            );
            current.details = details;
            misrecorded = record;
        }
        Ok(Some(LiveLogin {
            current,
            misrecorded,
            verified,
            record,
        }))
    }

    /// [`Engine::read_live`], without a Claude Code login it signed out of
    /// (its tokens blank, or nothing left to renew them): one holds nothing
    /// to park or keep from renewing, and nobody is using it.
    fn read_live_signed_in(&self, provider: ProviderId) -> Result<Option<LiveLogin>> {
        let now = now_epoch() as i64;
        Ok(self.read_live(provider)?.filter(|login| {
            provider != ProviderId::ClaudeCode
                || !matches!(
                    usage::claude_token(&login.current.blob, now),
                    Token::SignedOut
                )
        }))
    }

    /// Ask Anthropic whose token Claude Code's live login carries, unless
    /// that token was asked about already: one short request each time the
    /// token changes. Best effort: an answer that doesn't come leaves the
    /// login's record (or the last check, for a token it renewed) to say
    /// whose it is. Call it before anything that parks the live login.
    pub fn check_live(&self, api: &dyn UsageApi) {
        let pid = ProviderId::ClaudeCode;
        let Ok(Some(current)) = provider_for(pid).read_active(&self.host) else {
            return;
        };
        let Some(token) = usage::claude_access_token(&current.blob) else {
            return;
        };
        let print = live::fingerprint(&token);
        if live::checked(&self.host, pid).is_some_and(|c| c.token == print) {
            return;
        }
        // A lapsed token is renewed by Claude Code, and the new one asked about.
        if !matches!(
            usage::claude_token(&current.blob, now_epoch() as i64),
            Token::Valid(_)
        ) {
            return;
        }
        let Some((identity, label)) = api
            .claude_profile(&token)
            .ok()
            .and_then(|profile| usage::profile_owner(&profile))
        else {
            return;
        };
        self.keep_check(print, &current.details, identity, label);
    }

    /// Keep Anthropic's answer that the token `print`, read with `record`,
    /// is `identity`'s, unless Claude Code holds another token by now: a
    /// switch while Anthropic was asked put another login in place, and the
    /// answer, of one that's gone, would be taken for the new one's.
    fn keep_check(&self, print: String, record: &Details, identity: String, label: Option<String>) {
        let pid = ProviderId::ClaudeCode;
        let still = provider_for(pid)
            .read_active(&self.host)
            .ok()
            .flatten()
            .and_then(|now| usage::claude_access_token(&now.blob))
            .is_some_and(|t| live::fingerprint(&t) == print);
        if !still {
            return;
        }
        let recorded = live::misrecorded_as(record, &identity, label.as_deref());
        let _ = live::record_check(
            &self.host,
            pid,
            live::Checked {
                token: print,
                identity,
                label,
                recorded,
            },
        );
    }

    pub fn enroll(&self, provider: ProviderId, name: &str) -> Result<AccountMeta> {
        validate_name(name)?;
        let mut state = State::load(&self.host)?;
        refuse_taken_name(&state, provider, name, None)?;
        let LiveLogin {
            current: active,
            misrecorded,
            ..
        } = self
            .read_live(provider)?
            .ok_or_else(|| Error::NotLoggedIn(provider.display_name().into()))?;
        if let Some(existing) = live::enrolled_as(&state, provider, &active.details) {
            return Err(Error::AlreadyEnrolled {
                name: existing,
                provider: provider.display_name().into(),
            });
        }
        // Parked as it is, switching back would bring back a record naming
        // someone else, and there is no record of its own to park it with.
        if let Some(record) = misrecorded {
            return Err(Error::Misrecorded {
                tool: provider.display_name().into(),
                label: active.details.label,
                recorded: record.label,
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
        let (current, misrecorded, verified) = match self.read_live_signed_in(pid)? {
            // Unchecked, it is whoever its record names: a check of the token
            // it renewed may be of a login `/login` replaced since.
            Some(LiveLogin {
                mut current,
                misrecorded,
                verified,
                record,
            }) => {
                // A check of the token it renewed found it someone else's
                // than its record says: whose this one is waits to be asked.
                if !verified && misrecorded.is_some() {
                    return Err(Error::Unverified {
                        tool: pid.display_name().into(),
                    });
                }
                if !verified {
                    current.details = record;
                }
                (Some(current), verified && misrecorded.is_some(), verified)
            }
            None => (None, false, true),
        };
        let owner = current
            .as_ref()
            .and_then(|c| live::owner(&state, pid, &c.details));

        // Only the record says whose the live login is, and it names another
        // account than the one put in use: something else may have rewritten
        // it (the Claude desktop app writes its own account there). Gone by,
        // the token would be parked under the wrong account, or this switch
        // taken as made already; nothing moves until whose it is is known.
        let active = state.active_name(pid).filter(|active| {
            state
                .accounts
                .iter()
                .any(|a| a.provider == pid && &a.name == *active)
        });
        if current.is_some() && !verified && active.is_some() && owner.as_ref() != active {
            return Err(Error::Unverified {
                tool: pid.display_name().into(),
            });
        }

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
                    let id = vault_id(pid, owner);
                    let parked = if misrecorded {
                        // Its record names someone else: parked with the
                        // owner's own record, as last parked.
                        claude_code::with_record_of(&current.blob, &vault::load(&self.host, &id)?)?
                    } else {
                        current.blob.clone()
                    };
                    vault::store(&self.host, &id, &parked)?;
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
        // The login checked is gone; this one is the account's own.
        let _ = live::forget_check(&self.host, pid);
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
        // A login that doesn't say whose it is can't be told from someone
        // else's: it never replaces an account's.
        let anonymous = details.identity.is_none() && details.label.is_none();
        if anonymous || live::is_someone_else(&details, &meta) {
            return Err(Error::WrongAccount {
                name: meta.name,
                label: details.label,
            });
        }

        let current = self.read_live(pid)?.map(|login| login.current);
        let in_use = current
            .as_ref()
            .and_then(|c| live::owner(&state, pid, &c.details))
            .as_deref()
            == Some(meta.name.as_str());
        // An open editor would never see the new login, and would write the
        // old one back: refused before anything changes.
        if in_use && self.host.editor_running(pid) {
            return Err(Error::EditorStillOpen(pid.display_name().into()));
        }
        vault::store(&self.host, &vault_id(pid, &meta.name), blob)?;
        if in_use {
            if let Err(e) = prov.write_active(&self.host, blob) {
                if let Some(current) = &current {
                    let _ = prov.write_active(&self.host, &current.blob);
                }
                return Err(e);
            }
            state.active.insert(pid.as_str().into(), meta.name.clone());
            let _ = live::forget_check(&self.host, pid);
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
    /// run in parallel. A parked Claude Code token is first asked whose it
    /// is, once each time it changes: one found to be another account's is
    /// [`UsageReport::Unauthorized`], so it asks for a new sign-in and never
    /// shows someone else's numbers. Reading changes no login or account
    /// (only the checks of whose Claude Code's tokens are); see
    /// [`Engine::record_plans`].
    pub fn usage(&self, api: &dyn UsageApi) -> Result<Vec<AccountUsage>> {
        let state = State::load(&self.host)?;
        // Whose token Claude Code holds decides whose numbers it reads.
        if state
            .accounts
            .iter()
            .any(|a| a.provider == ProviderId::ClaudeCode)
        {
            self.check_live(api);
        }
        let now = now_epoch() as i64;

        // Each tool's live login and the account it belongs to. A login that
        // cannot be read, or that nobody enrolled owns, is read for nobody.
        let mut live_logins: Vec<(ProviderId, String, Vec<u8>)> = Vec::new();
        for pid in ProviderId::ALL {
            if !state.accounts.iter().any(|a| a.provider == pid) {
                continue;
            }
            if let Ok(Some(LiveLogin { current, .. })) = self.read_live(pid) {
                if let Some(owner) = live::owner(&state, pid, &current.details) {
                    live_logins.push((pid, owner, current.blob));
                }
            }
        }

        // Each account's request, with the last check of whose its token is
        // for a parked Claude Code login (`Some(None)`: never checked).
        type Planned<'a> = (
            &'a AccountMeta,
            std::result::Result<Request, UsageReport>,
            Option<Option<live::Checked>>,
        );
        let requests: Vec<Planned> = state
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
                let parked_claude = live.is_none() && a.provider == ProviderId::ClaudeCode;
                (
                    a,
                    blob.and_then(|blob| Request::for_blob(a.provider, &blob, now)),
                    parked_claude.then(|| live::parked_check(&self.host, a.provider, &a.name)),
                )
            })
            .collect();

        let (read, found): (Vec<AccountUsage>, Vec<Option<(String, live::Checked)>>) =
            std::thread::scope(|scope| {
                let handles: Vec<_> = requests
                    .into_iter()
                    .map(|(meta, request, check)| {
                        let handle = request.map(|request| {
                            scope.spawn(move || match check {
                                Some(last) => vetted(meta, request, last, api),
                                None => (request.send(api), None),
                            })
                        });
                        (meta, handle)
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(meta, handle)| {
                        let (report, found) = match handle {
                            Ok(h) => h.join().unwrap_or_else(|_| {
                                (UsageReport::Failed("request panicked".into()), None)
                            }),
                            Err(report) => (report, None),
                        };
                        let usage = AccountUsage {
                            provider: meta.provider,
                            name: meta.name.clone(),
                            report,
                        };
                        (usage, found.map(|check| (meta.name.clone(), check)))
                    })
                    .unzip()
            });
        let claude: Vec<&str> = state
            .accounts
            .iter()
            .filter(|a| a.provider == ProviderId::ClaudeCode)
            .map(|a| a.name.as_str())
            .collect();
        // Best effort: a check not kept is asked again next time.
        let _ = live::record_parked_checks(
            &self.host,
            ProviderId::ClaudeCode,
            found.into_iter().flatten().collect(),
            &claude,
        );
        Ok(read)
    }

    /// Renew each parked Claude Code login whose access token has lapsed, or
    /// is about to, as Claude Code would: its refresh token buys new tokens,
    /// which go back in the vault. Otherwise its usage could not be read until
    /// a switch or a warm-up renewed it, hours later.
    ///
    /// Only parked logins: the vault alone holds them, so nobody renews the
    /// same chain meanwhile. The live login is Claude Code's to renew: its
    /// owner is left alone, and so is any parked copy holding its refresh
    /// token, whoever that copy says it belongs to. When the live login can't
    /// be read nothing is renewed, since it may be any of them. A lapsed login
    /// holding nothing to renew it with is [`Renewal::Refused`].
    /// `wanted` picks the lapsed accounts to try. The caller keeps switches
    /// out while this runs: a switch would put in use a login being renewed.
    pub fn renew_parked(
        &self,
        api: &dyn UsageApi,
        wanted: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<(String, Renewal)>> {
        let provider = ProviderId::ClaudeCode;
        let state = State::load(&self.host)?;
        // Whose token Claude Code holds, not its record, is the one in use: a
        // record another app rewrote would leave the real one to be renewed.
        self.check_live(api);
        let live = self.read_live_signed_in(provider)?;
        let live_chain = live
            .as_ref()
            .and_then(|login| usage::claude_refresh_token(&login.current.blob));
        let parked: Vec<(&AccountMeta, Vec<u8>)> = state
            .accounts
            .iter()
            .filter(|a| a.provider == provider)
            .filter_map(|a| {
                Some((
                    a,
                    vault::load(&self.host, &vault_id(provider, &a.name)).ok()?,
                ))
            })
            .collect();
        let mut in_use: Vec<String> = Vec::new();
        if let Some(login) = &live {
            in_use.extend(live::owner(&state, provider, &login.current.details));
            // Unchecked, and its chain in no parked copy to say whose it is:
            // its record may have been rewritten by another app, so the
            // account it names and the one put in use may each be in use,
            // their parked copies Claude Code's old chain.
            let chain_parked = live_chain.is_some()
                && parked
                    .iter()
                    .any(|(_, blob)| usage::claude_refresh_token(blob) == live_chain);
            if !login.verified && !chain_parked {
                in_use.extend(live::owner(&state, provider, &login.record));
                in_use.extend(state.active_name(provider).cloned());
            }
        }
        let mut renewed = Vec::new();
        for (meta, blob) in parked {
            if in_use.contains(&meta.name) || !wanted(&meta.name) {
                continue;
            }
            let id = vault_id(provider, &meta.name);
            if live_chain.is_some() && usage::claude_refresh_token(&blob) == live_chain {
                continue;
            }
            let Some(renewal) =
                usage::claude_renewal(&blob, now_epoch() as i64, usage::RENEW_AHEAD)
            else {
                if matches!(
                    usage::claude_token(&blob, now_epoch() as i64),
                    Token::Expired
                ) {
                    renewed.push((meta.name.clone(), Renewal::Refused));
                }
                continue;
            };
            let outcome = match api.renew_claude(&renewal) {
                Ok(answer) => match usage::claude_renewed(&blob, &answer, now_epoch() as i64) {
                    Some(blob) => match vault::store(&self.host, &id, &blob) {
                        Ok(()) => Renewal::Renewed,
                        Err(e) => Renewal::Failed(format!("couldn't keep the renewed login: {e}")),
                    },
                    None => Renewal::Failed("Anthropic's answer held no new token".into()),
                },
                Err(usage::FetchError::Unauthorized) => Renewal::Refused,
                Err(usage::FetchError::Other(why)) => Renewal::Failed(why),
            };
            renewed.push((meta.name.clone(), outcome));
        }
        Ok(renewed)
    }

    /// Where a [`Engine::usage`] read names the plan (Cursor's does), record
    /// it over the one recorded when the login was parked, which may be out
    /// of date. State is loaded afresh, so call this as one more change: one
    /// at a time with the others. Failing to save only leaves the old plan.
    pub fn record_plans(&self, read: &[AccountUsage]) {
        let named: Vec<(ProviderId, &str, &str)> = read
            .iter()
            .filter_map(|u| match &u.report {
                UsageReport::Ok(usage) => {
                    Some((u.provider, u.name.as_str(), usage.plan.as_deref()?))
                }
                _ => None,
            })
            .collect();
        if named.is_empty() {
            return;
        }
        let Ok(mut state) = State::load(&self.host) else {
            return;
        };
        let mut changed = false;
        for (pid, name, plan) in named {
            let meta = state
                .accounts
                .iter_mut()
                .find(|a| a.provider == pid && a.name == name);
            if let Some(meta) = meta.filter(|m| m.plan.as_deref() != Some(plan)) {
                meta.plan = Some(plan.to_owned());
                changed = true;
            }
        }
        if changed {
            let _ = state.save(&self.host);
        }
    }
}

/// A tool's live login as [`Engine::read_live`] finds it. Deliberately not
/// `Debug`: it holds the login.
struct LiveLogin {
    /// Its details those of the account the last check found the token to be.
    current: ActiveCredential,
    /// The login's own record, where that check found the token to be
    /// someone else's.
    misrecorded: Option<Details>,
    /// Whether a check of this very token says whose it is, not the record
    /// (nor a check of the token it renewed). Always, for a tool whose login
    /// itself says whose it is.
    verified: bool,
    /// The login's own details, before any check.
    record: Details,
}

/// One usage request, with the token it carries. Deliberately not `Debug`.
enum Request {
    Claude(String),
    Codex {
        access_token: String,
        account_id: String,
    },
    Cursor(String),
    /// Known without asking anyone.
    Known(UsageReport),
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
                Token::SignedOut => Err(UsageReport::Unauthorized),
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
            ProviderId::Cursor => match crate::provider::cursor::access_token(blob) {
                Some((_, Some(exp))) if exp <= now => Err(UsageReport::Expired),
                Some((token, _)) => Ok(Request::Cursor(token)),
                None => Err(missing()),
            },
            ProviderId::Antigravity => Ok(Request::Known(
                crate::provider::antigravity::quota(blob)
                    .map(UsageReport::Ok)
                    .unwrap_or_else(|| {
                        UsageReport::Failed(
                            "Antigravity hasn't read this account's quota yet".into(),
                        )
                    }),
            )),
        }
    }

    fn send(self, api: &dyn UsageApi) -> UsageReport {
        match self {
            Request::Claude(token) => usage::claude_report(api.claude(&token)),
            Request::Codex {
                access_token,
                account_id,
            } => usage::codex_report(api.codex(&access_token, &account_id)),
            Request::Cursor(token) => usage::cursor_report(api.cursor(&token)),
            Request::Known(report) => report,
        }
    }
}

/// The read of `meta`'s parked Claude Code login, unless its token is found
/// to be another account's: Anthropic is asked whose it is once per token,
/// `last` being the last answer. A new answer comes back to be kept; none
/// (Anthropic out of reach) leaves the read to go ahead.
fn vetted(
    meta: &AccountMeta,
    request: Request,
    last: Option<live::Checked>,
    api: &dyn UsageApi,
) -> (UsageReport, Option<live::Checked>) {
    let Request::Claude(token) = &request else {
        return (request.send(api), None);
    };
    let print = live::fingerprint(token);
    let (check, new) = match last.filter(|c| c.token == print) {
        Some(known) => (Some(known), false),
        None => {
            let asked = api
                .claude_profile(token)
                .ok()
                .and_then(|profile| usage::profile_owner(&profile))
                .map(|(identity, label)| live::Checked {
                    token: print,
                    identity,
                    label,
                    recorded: None,
                });
            (asked, true)
        }
    };
    let someone_else = check.as_ref().is_some_and(|c| {
        let found = Details {
            label: c.label.clone(),
            identity: Some(c.identity.clone()),
            plan: None,
        };
        live::is_someone_else(&found, meta)
    });
    let report = if someone_else {
        UsageReport::Unauthorized
    } else {
        request.send(api)
    };
    (report, check.filter(|_| new))
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
                plan: None,
                recorded: None,
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
                plan: None,
                recorded: None,
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
        // as the app and the command line do before a switch
        engine.check_live(&FakeApi::new().with_owner("SECRET-TOKEN-B-FRESH", "b@x.com"));
        engine.switch("work", None).unwrap();
        assert!(active_token(engine.host()).contains(TOKEN_A));
        assert!(parked(&engine, "personal").contains("SECRET-TOKEN-B-FRESH"));
        assert!(parked(&engine, "work").contains(TOKEN_A));
    }

    #[test]
    fn switch_never_parks_an_unmanaged_login_in_another_accounts_place() {
        let engine = work_and_personal();
        sign_in_as(engine.host(), TOKEN_C, "c@x.com");
        engine.check_live(&FakeApi::new().with_owner(TOKEN_C, "c@x.com"));
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
        sign_in_as(engine.host(), TOKEN_C, "c@x.com");
        engine.check_live(&FakeApi::new().with_owner(TOKEN_C, "c@x.com"));
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
        engine.check_live(&FakeApi::new());
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
        /// The tokens asked whose they are.
        profiled: std::sync::Mutex<Vec<String>>,
        /// Whose each token is, by email; one not here gets no answer.
        owners: Vec<(&'static str, &'static str)>,
    }

    impl FakeApi {
        fn new() -> Self {
            Self {
                seen: std::sync::Mutex::new(Vec::new()),
                profiled: std::sync::Mutex::new(Vec::new()),
                owners: vec![
                    (TOKEN_A, "a@x.com"),
                    (TOKEN_B, "b@x.com"),
                    ("SECRET-TOKEN-B2", "b@x.com"),
                ],
            }
        }

        /// One where `token` is `email`'s, whatever the login's record says.
        fn with_owner(mut self, token: &'static str, email: &'static str) -> Self {
            self.owners.retain(|(t, _)| *t != token);
            self.owners.push((token, email));
            self
        }
    }

    impl UsageApi for FakeApi {
        /// Shaped as Anthropic answers, with the identity `config` records.
        fn claude_profile(
            &self,
            token: &str,
        ) -> std::result::Result<serde_json::Value, usage::FetchError> {
            self.profiled.lock().unwrap().push(token.to_owned());
            let (_, email) = self
                .owners
                .iter()
                .find(|(t, _)| *t == token)
                .ok_or_else(|| usage::FetchError::Other("no answer".into()))?;
            Ok(serde_json::json!({
                "account": {"uuid": format!("uuid-{email}"), "email": email},
                "organization": {"uuid": "org-1", "name": "Org"}
            }))
        }

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

        fn cursor(&self, token: &str) -> std::result::Result<serde_json::Value, usage::FetchError> {
            self.seen.lock().unwrap().push(format!("cursor|{token}"));
            Ok(serde_json::json!({
                "planUsage": {"totalPercentUsed": 42},
                "planInfo": {"planName": "Plus"}
            }))
        }

        fn renew_claude(
            &self,
            renewal: &usage::ClaudeRenewal,
        ) -> std::result::Result<serde_json::Value, usage::FetchError> {
            let refresh = &renewal.refresh_token;
            self.seen.lock().unwrap().push(format!("renew|{refresh}"));
            if refresh == "SECRET-RT-DEAD" {
                return Err(usage::FetchError::Unauthorized);
            }
            Ok(serde_json::json!({
                "access_token": format!("{refresh}-NEW-AT"),
                "refresh_token": format!("{refresh}-NEW-RT"),
                "expires_in": 28_800
            }))
        }
    }

    /// Claude Code's credentials for a login whose access token lapses at
    /// `expires_at` (epoch seconds), renewed with `refresh`.
    fn lapsing_creds(token: &str, refresh: &str, expires_at: i64) -> String {
        serde_json::json!({"claudeAiOauth": {
            "accessToken": token,
            "refreshToken": refresh,
            "expiresAt": expires_at * 1000,
            "scopes": ["user:inference", "user:profile"]
        }})
        .to_string()
    }

    fn parked_token(engine: &Engine<FakeHost>, name: &str) -> Option<String> {
        let blob = vault::load(engine.host(), &vault_id(ProviderId::ClaudeCode, name)).unwrap();
        match usage::claude_token(&blob, now_epoch() as i64) {
            Token::Valid(token) => Some(token),
            _ => None,
        }
    }

    /// "work" parked and "personal" in use, both with tokens that lapsed an
    /// hour ago; "work" renews with `work_refresh`.
    fn parked_and_in_use_lapsed(work_refresh: &str) -> Engine<FakeHost> {
        let lapsed = now_epoch() as i64 - 3_600;
        let host = FakeHost::new();
        host.put_file(
            ClaudeCode::credentials_path(&host),
            lapsing_creds(TOKEN_A, work_refresh, lapsed),
        );
        host.put_file(ClaudeCode::config_path(&host), config("a@x.com"));
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            lapsing_creds(TOKEN_B, "SECRET-RT-B", lapsed),
        );
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("b@x.com"));
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();
        engine
    }

    #[test]
    fn a_parked_login_whose_token_lapsed_is_renewed_and_the_one_in_use_never() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        let live_before = active_token(engine.host());

        let api = FakeApi::new();
        let renewed = engine.renew_parked(&api, &|_| true).unwrap();

        assert_eq!(renewed, [("work".to_owned(), Renewal::Renewed)]);
        assert_eq!(api.seen.lock().unwrap().as_slice(), ["renew|SECRET-RT-A"]);
        assert_eq!(
            parked_token(&engine, "work").as_deref(),
            Some("SECRET-RT-A-NEW-AT")
        );
        // Claude Code's own login, and the vault copy of it, are left alone.
        assert_eq!(active_token(engine.host()), live_before);
        assert_eq!(parked_token(&engine, "personal"), None);

        // Renewed, it reads; and it isn't renewed again.
        let all = engine.usage(&api).unwrap();
        assert_eq!(report_for(&all, "personal"), &UsageReport::Expired);
        assert!(api
            .seen
            .lock()
            .unwrap()
            .contains(&"SECRET-RT-A-NEW-AT".to_owned()));
        assert!(engine.renew_parked(&api, &|_| true).unwrap().is_empty());
    }

    #[test]
    fn a_refused_renewal_leaves_the_parked_login_as_it_was() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-DEAD");
        let before = vault::load(engine.host(), &vault_id(ProviderId::ClaudeCode, "work")).unwrap();

        let renewed = engine.renew_parked(&FakeApi::new(), &|_| true).unwrap();

        assert_eq!(renewed, [("work".to_owned(), Renewal::Refused)]);
        assert_eq!(
            vault::load(engine.host(), &vault_id(ProviderId::ClaudeCode, "work")).unwrap(),
            before
        );
    }

    #[test]
    fn nothing_is_renewed_while_the_live_login_cannot_be_read() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), "{not json");

        let api = FakeApi::new();
        assert!(engine.renew_parked(&api, &|_| true).is_err());
        assert!(api.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn with_claude_code_signed_out_every_lapsed_login_is_parked_and_renewed() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        engine
            .host()
            .remove_file(&ClaudeCode::credentials_path(engine.host()))
            .unwrap();

        let mut renewed = engine.renew_parked(&FakeApi::new(), &|_| true).unwrap();
        renewed.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            renewed,
            [
                ("personal".to_owned(), Renewal::Renewed),
                ("work".to_owned(), Renewal::Renewed)
            ]
        );
    }

    #[test]
    fn a_parked_copy_of_the_live_chain_is_never_renewed_whoever_it_says_it_is() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        // Claude Code now holds work's chain under an identity nobody owns.
        let lapsed = now_epoch() as i64 - 3_600;
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            lapsing_creds(TOKEN_A, "SECRET-RT-A", lapsed),
        );
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("c@x.com"));

        let api = FakeApi::new();
        let renewed = engine.renew_parked(&api, &|_| true).unwrap();
        assert_eq!(renewed, [("personal".to_owned(), Renewal::Renewed)]);
        assert_eq!(api.seen.lock().unwrap().as_slice(), ["renew|SECRET-RT-B"]);
    }

    #[test]
    fn the_account_whose_token_claude_code_holds_is_never_renewed_whatever_its_record_says() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        // Claude Code renewed personal's login, and another app signed in to
        // work (the Claude desktop app, sharing ~/.claude.json) rewrote the
        // record to work's. The parked copy of personal is the old chain.
        let later = now_epoch() as i64 + 3_600;
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            lapsing_creds("SECRET-TOKEN-B2", "SECRET-RT-B2", later),
        );
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("a@x.com"));

        let api = FakeApi::new();
        let renewed = engine.renew_parked(&api, &|_| true).unwrap();

        assert_eq!(renewed, [("work".to_owned(), Renewal::Renewed)]);
        assert_eq!(api.seen.lock().unwrap().as_slice(), ["renew|SECRET-RT-A"]);
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("personal".into())
        );
    }

    #[test]
    fn a_lapsed_login_with_nothing_to_renew_it_needs_signing_in_again() {
        let lapsed = now_epoch() as i64 - 3_600;
        let host = FakeHost::new();
        host.put_file(
            ClaudeCode::credentials_path(&host),
            serde_json::json!({"claudeAiOauth": {
                "accessToken": TOKEN_A, "expiresAt": lapsed * 1000
            }})
            .to_string(),
        );
        host.put_file(ClaudeCode::config_path(&host), config("a@x.com"));
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();

        let api = FakeApi::new();
        let renewed = engine.renew_parked(&api, &|_| true).unwrap();
        assert_eq!(renewed, [("work".to_owned(), Renewal::Refused)]);
        assert!(api.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn only_the_wanted_lapsed_logins_are_renewed() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-A");
        let api = FakeApi::new();
        assert!(engine
            .renew_parked(&api, &|name| name != "work")
            .unwrap()
            .is_empty());
        assert!(api.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_parked_token_still_good_for_a_while_is_not_renewed() {
        let later = now_epoch() as i64 + 3_600;
        let host = FakeHost::new();
        host.put_file(
            ClaudeCode::credentials_path(&host),
            lapsing_creds(TOKEN_A, "SECRET-RT-A", later),
        );
        host.put_file(ClaudeCode::config_path(&host), config("a@x.com"));
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.enroll(ProviderId::ClaudeCode, "personal").unwrap();

        let api = FakeApi::new();
        assert!(engine.renew_parked(&api, &|_| true).unwrap().is_empty());
        assert!(api.seen.lock().unwrap().is_empty());
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
    fn a_login_claude_code_signed_out_reads_as_needing_a_sign_in_not_as_expired() {
        // What Claude Code leaves once a renewal is refused: no switch renews it.
        let blanked = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#;
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), blanked);
        host.put_file(ClaudeCode::config_path(&host), config("b@x.com"));
        let engine = Engine::new(host);
        engine.enroll(ProviderId::ClaudeCode, "gone").unwrap();
        sign_in_as(engine.host(), TOKEN_A, "a@x.com");
        engine.enroll(ProviderId::ClaudeCode, "work").unwrap();

        let api = FakeApi::new();
        let all = engine.usage(&api).unwrap();
        assert_eq!(report_for(&all, "gone"), &UsageReport::Unauthorized);
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
        assert_eq!(api.seen.lock().unwrap().as_slice(), [TOKEN_A]);
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

    // ---- a token whose record names someone else

    const TOKEN_C: &str = "SECRET-TOKEN-CCC";

    /// What was seen on a Mac (2026-10): something signed Claude Code in to
    /// another account and rewrote only the token; `~/.claude.json` still
    /// names `work`.
    fn token_replaced_under_works_record(token: &str) -> Engine<FakeHost> {
        let engine = work_and_personal();
        engine
            .host()
            .put_file(ClaudeCode::credentials_path(engine.host()), creds(token));
        engine
    }

    #[test]
    fn a_token_found_to_be_someone_elses_is_not_read_as_the_recorded_account() {
        let engine = token_replaced_under_works_record(TOKEN_C);
        let api = FakeApi::new().with_owner(TOKEN_C, "c@x.com");

        let all = engine.usage(&api).unwrap();
        // work is read with its own parked token, not with c's
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
        assert!(!api.seen.lock().unwrap().iter().any(|t| t == TOKEN_C));
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Unmanaged {
                label: Some("c@x.com".into()),
                plan: None,
                recorded: Some("a@x.com".into()),
            }
        );
        // asked once for this token, not on every read
        engine.usage(&api).unwrap();
        let asked = api.profiled.lock().unwrap();
        assert_eq!(asked.iter().filter(|t| *t == TOKEN_C).count(), 1);
        let kept = String::from_utf8(
            engine
                .host()
                .file(&live::checked_path(engine.host()))
                .unwrap(),
        )
        .unwrap();
        assert!(!kept.contains(TOKEN_C));
    }

    #[test]
    fn a_token_found_to_be_someone_elses_is_never_parked_as_the_recorded_account() {
        let engine = token_replaced_under_works_record(TOKEN_C);
        engine.check_live(&FakeApi::new().with_owner(TOKEN_C, "c@x.com"));

        assert_eq!(
            engine.switch_keeping_unmanaged("personal", None).unwrap(),
            SwitchOutcome::WouldReplace(UnmanagedLogin {
                label: Some("c@x.com".into())
            })
        );
        assert!(matches!(
            engine.enroll(ProviderId::ClaudeCode, "third"),
            Err(Error::Misrecorded { label, recorded, .. })
                if label.as_deref() == Some("c@x.com") && recorded.as_deref() == Some("a@x.com")
        ));
        engine.switch("personal", None).unwrap();
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert!(!parked(&engine, "work").contains(TOKEN_C));
        // what was found of c's login doesn't stick to work's own
        engine.switch("work", None).unwrap();
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("work".into())
        );
    }

    #[test]
    fn a_token_claude_code_renews_under_the_same_wrong_record_stays_someone_elses() {
        let engine = token_replaced_under_works_record(TOKEN_C);
        engine.check_live(&FakeApi::new().with_owner(TOKEN_C, "c@x.com"));
        // renewed, and Anthropic can't be asked right now
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            creds("SECRET-TOKEN-C2"),
        );
        let api = FakeApi::new();
        let all = engine.usage(&api).unwrap();
        assert!(matches!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Unmanaged { label, recorded, .. }
                if label.as_deref() == Some("c@x.com") && recorded.as_deref() == Some("a@x.com")
        ));
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
        assert!(!api
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == "SECRET-TOKEN-C2"));
    }

    #[test]
    fn a_lapsed_token_is_not_asked_about() {
        let engine = work_and_personal();
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            r#"{"claudeAiOauth":{"accessToken":"SECRET-OLD","expiresAt":1000}}"#,
        );
        let api = FakeApi::new();
        engine.check_live(&api);
        assert!(api.profiled.lock().unwrap().is_empty());
    }

    #[test]
    fn a_token_found_to_be_another_accounts_parks_with_that_accounts_record() {
        let engine = token_replaced_under_works_record("SECRET-TOKEN-B2");
        engine.check_live(&FakeApi::new());
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("personal".into())
        );

        engine.switch("work", None).unwrap();
        let personal = parked(&engine, "personal");
        assert!(personal.contains("SECRET-TOKEN-B2"));
        assert!(personal.contains("b@x.com") && !personal.contains("a@x.com"));
        // and switching back brings back both halves of personal's login
        engine.switch("personal", None).unwrap();
        let config = engine
            .host()
            .file(&ClaudeCode::config_path(engine.host()))
            .unwrap();
        assert!(String::from_utf8(config).unwrap().contains("b@x.com"));
        assert!(active_token(engine.host()).contains("SECRET-TOKEN-B2"));
    }

    #[test]
    fn a_check_with_no_answer_leaves_the_record_to_say_whose_the_login_is() {
        let engine = token_replaced_under_works_record(TOKEN_C);
        // FakeApi doesn't know TOKEN_C
        let all = engine.usage(&FakeApi::new()).unwrap();
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("work".into())
        );
        assert_eq!(report_for(&all, "work"), &UsageReport::Unauthorized);
    }

    /// What was seen on a Mac (2026-11): `work` in use, and the Claude
    /// desktop app, signed in to `personal`, rewrote `~/.claude.json` to
    /// name it; the token is still work's.
    fn record_rewritten_to_personal() -> Engine<FakeHost> {
        let engine = work_and_personal();
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("b@x.com"));
        engine
    }

    #[test]
    fn a_switch_waits_while_a_record_naming_another_account_cant_be_checked() {
        let engine = record_rewritten_to_personal();
        // Anthropic out of reach: only the record says whose the login is
        let api_down = FakeApi {
            owners: Vec::new(),
            ..FakeApi::new()
        };
        engine.check_live(&api_down);

        for name in ["personal", "work"] {
            assert!(matches!(
                engine.switch(name, None),
                Err(Error::Unverified { .. })
            ));
        }
        // nothing moved: work's token stays put, and nobody's parked copy changed
        assert!(active_token(engine.host()).contains(TOKEN_A));
        assert!(parked(&engine, "personal").contains(TOKEN_B));
        assert_eq!(
            engine.state().unwrap().active_name(ProviderId::ClaudeCode),
            Some(&"work".to_owned())
        );

        // once Anthropic says the token is work's, the switch parks it there
        engine.check_live(&FakeApi::new());
        engine.switch("personal", None).unwrap();
        let work = parked(&engine, "work");
        assert!(work.contains(TOKEN_A) && work.contains("a@x.com") && !work.contains("b@x.com"));
        assert!(active_token(engine.host()).contains(TOKEN_B));
    }

    #[test]
    fn a_record_agreeing_with_the_account_in_use_needs_no_check_to_switch() {
        let engine = work_and_personal();
        let api_down = FakeApi {
            owners: Vec::new(),
            ..FakeApi::new()
        };
        engine.check_live(&api_down);
        engine.switch("personal", None).unwrap();
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert!(active_token(engine.host()).contains(TOKEN_B));
    }

    #[test]
    fn a_login_claude_code_signed_out_of_is_neither_parked_nor_in_the_way() {
        let engine = record_rewritten_to_personal();
        // Claude Code blanked work's login after a refused renewal
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#,
        );
        assert!(matches!(
            engine.switch("personal", None).unwrap(),
            SwitchOutcome::Switched { replaced: None, .. }
        ));
        // nobody's parked login was overwritten with the blank one
        assert!(parked(&engine, "work").contains(TOKEN_A));
        assert!(parked(&engine, "personal").contains(TOKEN_B));
        assert!(active_token(engine.host()).contains(TOKEN_B));
    }

    #[test]
    fn after_a_login_offline_a_check_of_an_older_token_decides_no_switch() {
        let engine = record_rewritten_to_personal();
        // work's token, checked under the desktop app's record
        engine.check_live(&FakeApi::new());
        // then `/login` to personal, with Anthropic out of reach
        sign_in_as(engine.host(), "SECRET-TOKEN-P9", "b@x.com");
        let api_down = FakeApi {
            owners: Vec::new(),
            ..FakeApi::new()
        };
        engine.check_live(&api_down);
        for name in ["work", "personal"] {
            assert!(matches!(
                engine.switch(name, None),
                Err(Error::Unverified { .. })
            ));
        }
        assert!(!parked(&engine, "work").contains("SECRET-TOKEN-P9"));

        // asked, it parks under personal
        engine.check_live(&FakeApi::new().with_owner("SECRET-TOKEN-P9", "b@x.com"));
        engine.switch("work", None).unwrap();
        assert!(parked(&engine, "personal").contains("SECRET-TOKEN-P9"));
        assert!(!parked(&engine, "work").contains("SECRET-TOKEN-P9"));
    }

    #[test]
    fn an_answer_about_a_token_a_switch_replaced_meanwhile_is_not_kept() {
        let engine = record_rewritten_to_personal();
        let asked_with = provider_for(ProviderId::ClaudeCode)
            .read_active(engine.host())
            .unwrap()
            .unwrap()
            .details;
        // the user switched to personal while Anthropic was being asked
        sign_in_as(engine.host(), TOKEN_B, "b@x.com");
        engine.keep_check(
            live::fingerprint(TOKEN_A),
            &asked_with,
            "uuid-a@x.com:org-1".into(),
            Some("a@x.com".into()),
        );
        assert!(live::checked(engine.host(), ProviderId::ClaudeCode).is_none());
        assert_eq!(
            engine.live(ProviderId::ClaudeCode).unwrap(),
            Live::Account("personal".into())
        );
    }

    #[test]
    fn an_unchecked_login_under_a_rewritten_record_renews_neither_account_it_may_be() {
        let engine = parked_and_in_use_lapsed("SECRET-RT-DEAD");
        engine.switch("work", None).unwrap();
        // Claude Code renewed work's chain and went idle; the desktop app
        // rewrote the record to personal. work's parked copy is the old chain.
        let lapsed = now_epoch() as i64 - 60;
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            lapsing_creds("SECRET-TOKEN-A2", "SECRET-RT-A2", lapsed),
        );
        engine
            .host()
            .put_file(ClaudeCode::config_path(engine.host()), config("b@x.com"));

        let api = FakeApi::new();
        let renewed = engine.renew_parked(&api, &|_| true).unwrap();
        assert!(renewed.is_empty(), "{renewed:?}");
        assert!(api.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_renewed_token_of_someone_unmanaged_waits_to_be_asked_before_a_switch() {
        let engine = token_replaced_under_works_record(TOKEN_C);
        engine.check_live(&FakeApi::new().with_owner(TOKEN_C, "c@x.com"));
        // Claude Code renewed c's login, still under work's record, offline
        engine.host().put_file(
            ClaudeCode::credentials_path(engine.host()),
            creds("SECRET-TOKEN-C2"),
        );
        let api_down = FakeApi {
            owners: Vec::new(),
            ..FakeApi::new()
        };
        engine.check_live(&api_down);
        assert!(matches!(
            engine.switch("personal", None),
            Err(Error::Unverified { .. })
        ));
        assert!(!parked(&engine, "work").contains("SECRET-TOKEN-C2"));

        // asked, it is c's: replaced, never parked under work
        engine.check_live(&FakeApi::new().with_owner("SECRET-TOKEN-C2", "c@x.com"));
        assert!(matches!(
            engine.switch("personal", None).unwrap(),
            SwitchOutcome::Switched {
                replaced: Some(_),
                ..
            }
        ));
        assert!(!parked(&engine, "work").contains("SECRET-TOKEN-C2"));
        assert!(parked(&engine, "work").contains(TOKEN_A));
    }

    /// `personal`'s parked login holding `token` under personal's own record.
    fn parked_as_personal(engine: &Engine<FakeHost>, token: &str) {
        let blob = serde_json::json!({
            "credentials": creds(token),
            "oauth_account": {
                "emailAddress": "b@x.com",
                "accountUuid": "uuid-b@x.com",
                "organizationUuid": "org-1"
            }
        });
        vault::store(
            engine.host(),
            &vault_id(ProviderId::ClaudeCode, "personal"),
            &serde_json::to_vec(&blob).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_parked_login_found_to_be_someone_elses_asks_for_a_sign_in_and_shows_none_of_their_numbers()
    {
        let engine = work_and_personal();
        // c's login, parked under personal by an older version
        parked_as_personal(&engine, TOKEN_C);
        let api = FakeApi::new().with_owner(TOKEN_C, "c@x.com");

        let all = engine.usage(&api).unwrap();
        assert_eq!(report_for(&all, "personal"), &UsageReport::Unauthorized);
        assert_eq!(five_hour(report_for(&all, "work")), 10.0);
        assert!(!api.seen.lock().unwrap().iter().any(|t| t == TOKEN_C));

        // asked once per token, and only a fingerprint is kept
        engine.usage(&api).unwrap();
        let asked = api.profiled.lock().unwrap().clone();
        assert_eq!(asked.iter().filter(|t| *t == TOKEN_C).count(), 1);
        let kept = engine
            .host()
            .file(&live::checked_path(engine.host()))
            .unwrap();
        assert!(!String::from_utf8(kept).unwrap().contains(TOKEN_C));

        // signed in again, personal's own login reads as before
        parked_as_personal(&engine, "SECRET-TOKEN-B2");
        let all = engine.usage(&api).unwrap();
        assert_eq!(five_hour(report_for(&all, "personal")), 20.0);
    }

    #[test]
    fn a_parked_login_whose_owner_cant_be_asked_is_read_as_before() {
        let engine = work_and_personal();
        parked_as_personal(&engine, "SECRET-TOKEN-B2");
        let api_down = FakeApi {
            owners: Vec::new(),
            ..FakeApi::new()
        };
        let all = engine.usage(&api_down).unwrap();
        assert_eq!(five_hour(report_for(&all, "personal")), 20.0);
        // nothing kept: asked again next time
        assert!(live::parked_check(engine.host(), ProviderId::ClaudeCode, "personal").is_none());
    }

    #[test]
    fn a_forgotten_accounts_parked_check_goes_with_it() {
        let engine = work_and_personal();
        engine.usage(&FakeApi::new()).unwrap();
        assert!(live::parked_check(engine.host(), ProviderId::ClaudeCode, "personal").is_some());
        engine.remove("personal", None).unwrap();
        engine.usage(&FakeApi::new()).unwrap();
        assert!(live::parked_check(engine.host(), ProviderId::ClaudeCode, "personal").is_none());
    }

    // ---- editors

    fn cursor_login(sub: &str, email: &str) -> Vec<u8> {
        use crate::provider::cursor::{tests::fake_jwt, ACCESS_TOKEN, EMAIL, REFRESH_TOKEN};
        let token = fake_jwt(&format!(r#"{{"sub":"{sub}","exp":4102444800}}"#));
        serde_json::to_vec(&std::collections::BTreeMap::from([
            (ACCESS_TOKEN, token.as_str()),
            (REFRESH_TOKEN, token.as_str()),
            (EMAIL, email),
        ]))
        .unwrap()
    }

    fn cursor_live(engine: &Engine<FakeHost>) -> Option<String> {
        provider_for(ProviderId::Cursor)
            .read_active(engine.host())
            .unwrap()
            .and_then(|a| a.details.label)
    }

    /// Cursor signed in to a@x.com, enrolled as `a`, and b@x.com added as `b`.
    fn two_cursor_accounts() -> Engine<FakeHost> {
        let host = FakeHost::new();
        crate::provider::cursor::tests::signed_in(&host, "auth0|user_A", "a@x.com", "pro");
        let engine = Engine::new(host);
        engine.enroll(ProviderId::Cursor, "a").unwrap();
        engine
            .add_signed_in(
                ProviderId::Cursor,
                "b",
                &cursor_login("auth0|user_B", "b@x.com"),
            )
            .unwrap();
        engine
    }

    #[test]
    fn a_read_that_names_the_plan_records_it() {
        // `a` was parked saying "pro", `b` signed in saying nothing
        let engine = two_cursor_accounts();
        let read = engine.usage(&FakeApi::new()).unwrap();
        // a read alone changes nothing
        let before = State::load(engine.host()).unwrap();
        assert_eq!(before.accounts[0].plan.as_deref(), Some("Pro"));
        engine.record_plans(&read);
        let state = State::load(engine.host()).unwrap();
        let plan = |name: &str| {
            let meta = state.accounts.iter().find(|a| a.name == name).unwrap();
            meta.plan.clone()
        };
        assert_eq!(plan("a").as_deref(), Some("Plus"));
        assert_eq!(plan("b").as_deref(), Some("Plus"));
    }

    #[test]
    fn a_closed_editor_switches_both_ways_and_keeps_each_login() {
        let engine = two_cursor_accounts();
        assert!(matches!(
            engine.switch("b", Some(ProviderId::Cursor)).unwrap(),
            SwitchOutcome::Switched { .. }
        ));
        assert_eq!(cursor_live(&engine).as_deref(), Some("b@x.com"));
        assert_eq!(
            engine.live(ProviderId::Cursor).unwrap(),
            Live::Account("b".into())
        );
        engine.switch("a", Some(ProviderId::Cursor)).unwrap();
        assert_eq!(cursor_live(&engine).as_deref(), Some("a@x.com"));
    }

    #[test]
    fn an_open_editor_is_never_switched_under_it() {
        let engine = two_cursor_accounts();
        engine.host().set_editor_open(ProviderId::Cursor, true);
        assert!(matches!(
            engine.switch("b", Some(ProviderId::Cursor)),
            Err(Error::EditorStillOpen(_))
        ));
        assert_eq!(cursor_live(&engine).as_deref(), Some("a@x.com"));
        assert_eq!(
            engine.live(ProviderId::Cursor).unwrap(),
            Live::Account("a".into())
        );
    }

    #[test]
    fn signing_in_again_to_an_open_editors_account_changes_nothing() {
        let engine = two_cursor_accounts();
        engine.host().set_editor_open(ProviderId::Cursor, true);
        let parked_before = vault::load(engine.host(), &vault_id(ProviderId::Cursor, "a")).unwrap();
        let again = cursor_login("auth0|user_A", "a@x.com");
        assert!(matches!(
            engine.sign_in_again("a", Some(ProviderId::Cursor), &again),
            Err(Error::EditorStillOpen(_))
        ));
        assert_eq!(
            vault::load(engine.host(), &vault_id(ProviderId::Cursor, "a")).unwrap(),
            parked_before
        );
        // A parked account isn't in use: its login is replaced.
        engine
            .sign_in_again(
                "b",
                Some(ProviderId::Cursor),
                &cursor_login("auth0|user_B", "b@x.com"),
            )
            .unwrap();
    }

    #[test]
    fn a_login_that_says_nothing_of_whose_it_is_never_replaces_an_account() {
        let engine = two_cursor_accounts();
        let anonymous = serde_json::to_vec(&std::collections::BTreeMap::from([(
            crate::provider::cursor::ACCESS_TOKEN,
            "not-a-jwt",
        )]))
        .unwrap();
        assert!(matches!(
            engine.sign_in_again("b", Some(ProviderId::Cursor), &anonymous),
            Err(Error::WrongAccount { .. })
        ));
    }
}
