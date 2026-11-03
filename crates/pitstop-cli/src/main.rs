use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pitstop_core::activity::{self, verb, By, Entry};
use pitstop_core::doctor::{self, Level};
use pitstop_core::engine::{AccountUsage, RemoveOutcome, SwitchOutcome};
use pitstop_core::live::Live;
use pitstop_core::usage::Http;
use pitstop_core::{signin, Engine, ProviderId, RealHost, State};

/// Park and switch Claude Code, Codex, Cursor and Antigravity accounts without signing out.
#[derive(Parser)]
#[command(name = "piaz-switch", version, about)]
struct Cli {
    /// Treat this directory as the home directory (sandbox/testing).
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Park the account the tool is signed in to, under a name; with
    /// --sign-in, sign in to another account instead, leaving that one alone.
    Enroll {
        /// Account name (letters, digits, - and _)
        name: String,
        /// claude-code, codex, cursor or antigravity
        #[arg(short, long)]
        provider: String,
        /// Sign in to a new account through the tool's own sign-in, in a
        /// private directory, without touching the login in use.
        #[arg(long)]
        sign_in: bool,
    },
    /// Show enrolled accounts, which one is active, and their usage.
    List {
        /// Skip asking the services for usage.
        #[arg(long)]
        offline: bool,
    },
    /// Switch the tool to a parked account.
    Use {
        name: String,
        /// Disambiguate when the same name exists for several providers.
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Give an account a new name, keeping its parked login.
    Rename {
        name: String,
        new_name: String,
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Forget an account and delete its parked credential.
    Remove {
        name: String,
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Show the last changes made to the accounts, from the app and here.
    Log {
        /// How many changes to show, newest first.
        #[arg(short = 'n', long, default_value_t = 30)]
        count: usize,
    },
    /// Check what Piaz Switch relies on: its folder, the vault key, each
    /// tool's program, the login in use and every parked login.
    Doctor,
}

fn parse_provider(s: Option<&str>) -> Result<Option<ProviderId>> {
    Ok(match s {
        None => None,
        Some(s) => Some(ProviderId::parse(s)?),
    })
}

fn print_list(state: &State, usage: &[AccountUsage]) {
    if state.accounts.is_empty() {
        println!("No accounts enrolled yet. Park the login a tool is using with:");
        println!("  piaz-switch enroll <name> --provider claude-code|codex|cursor|antigravity");
        println!("or sign in to another account with --sign-in.");
        return;
    }
    for provider in ProviderId::ALL {
        let accounts: Vec<_> = state
            .accounts
            .iter()
            .filter(|a| a.provider == provider)
            .collect();
        if accounts.is_empty() {
            continue;
        }
        println!("{}:", provider.display_name());
        for account in accounts {
            let mark = if state.active_name(provider) == Some(&account.name) {
                "*"
            } else {
                " "
            };
            let label = account.label.as_deref().unwrap_or("-");
            let plan = account
                .plan
                .as_deref()
                .map(|p| format!(" · {p}"))
                .unwrap_or_default();
            println!("  {mark} {:<20} {label}{plan}", account.name);
            if let Some(u) = usage
                .iter()
                .find(|u| u.provider == provider && u.name == account.name)
            {
                println!("      {}", u.report.summary(now_epoch()));
            }
        }
    }
    println!("\n(* = active)");
}

/// Record a change in the activity log; a log that can't be written stops
/// nothing.
fn record<T>(host: &RealHost, entry: Entry, result: &pitstop_core::error::Result<T>) {
    let _ = activity::record(host, &entry.outcome(result));
}

/// The one tool an account name belongs to, when only one has it.
fn owner(
    engine: &Engine<RealHost>,
    name: &str,
    provider: Option<ProviderId>,
) -> Option<ProviderId> {
    let state = engine.state().ok()?;
    let mut owners = state
        .accounts
        .iter()
        .filter(|a| a.name == name && provider.is_none_or(|p| p == a.provider));
    let first = owners.next()?.provider;
    owners.next().is_none().then_some(first)
}

/// The account a tool is using before a switch: its login's owner, or the
/// record when the login can't be read.
fn in_use(engine: &Engine<RealHost>, provider: ProviderId) -> Option<String> {
    match engine.live(provider) {
        Ok(Live::Account(name)) => Some(name),
        Ok(_) => None,
        Err(_) => engine
            .state()
            .ok()
            .and_then(|s| s.active_name(provider).cloned()),
    }
}

fn print_log(host: &RealHost, count: usize) -> Result<()> {
    let log = activity::recent(host, count)?;
    if log.is_empty() {
        println!("Nothing has been changed yet.");
        return Ok(());
    }
    for entry in log {
        let at = chrono::DateTime::from_timestamp(entry.at, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let tool = entry
            .provider
            .as_deref()
            .and_then(|p| ProviderId::parse(p).ok())
            .map(|p| format!(" ({})", p.display_name()))
            .unwrap_or_default();
        let what = match (entry.from.as_deref(), entry.account.as_deref()) {
            (Some(from), Some(to)) => format!("{from} -> {to}"),
            (None, Some(name)) => name.to_owned(),
            _ => String::new(),
        };
        let by = match entry.by {
            By::App => "app",
            By::Cli => "command line",
        };
        let result = if entry.worked() {
            String::new()
        } else {
            format!("  [{}]", entry.result)
        };
        println!("{at}  {:<14} {what}{tool}  by {by}{result}", entry.verb);
    }
    Ok(())
}

/// The doctor's report; true when no check failed.
fn print_doctor(engine: &Engine<RealHost>) -> bool {
    let home = dirs::home_dir().unwrap_or_default();
    let checks = doctor::run(engine, &|p| signin::find_program(p, &home));
    let mut group = String::new();
    for check in &checks {
        if check.group != group {
            group.clone_from(&check.group);
            let title = ProviderId::parse(&group)
                .map(|p| p.display_name().to_owned())
                .unwrap_or_else(|_| "Piaz Switch".into());
            println!("{title}");
        }
        let mark = match check.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("  {mark}  {}", doctor_line(check));
    }
    let (warn, fail) = doctor::issues(&checks);
    println!();
    match (warn + fail, fail) {
        (0, _) => println!("No issues. Everything Piaz Switch checks is in order."),
        (n, 0) => println!("{n} issue(s). Nothing here stops a switch."),
        (n, _) => {
            println!("{n} issue(s). Don't switch accounts until the failed checks are fixed.")
        }
    }
    fail == 0
}

fn doctor_line(check: &doctor::Check) -> String {
    use doctor::{check as id, found};
    let subject = check.subject.as_deref().unwrap_or("");
    match (check.id, check.finding.as_str()) {
        (id::FOLDER, found::WRITABLE) => format!("Folder {subject} can be written to"),
        (id::FOLDER, code) => format!("Folder {subject} can't be written to ({code})"),
        (id::STATE, found::READ) => format!("Account list read: {subject} account(s)"),
        (id::STATE, code) => format!("Account list can't be read ({code})"),
        (id::VAULT_KEY, found::PRESENT) => "Vault key is in the keystore".into(),
        (id::VAULT_KEY, code) => format!(
            "Vault key can't be used ({code}); parked logins can't be opened: remove every account, then enroll each again"
        ),
        (id::PROGRAM, found::FOUND) => format!("Program found: {subject}"),
        (id::PROGRAM, _) => "Program not found on this computer".into(),
        (id::LOGIN, found::SIGNED_IN) => format!("Signed in as `{subject}`"),
        (id::LOGIN, found::UNMANAGED) => {
            "Signed in to an account Piaz Switch doesn't manage; enroll it under a name".into()
        }
        (id::LOGIN, found::LOGGED_OUT) => "Signed in to nobody".into(),
        (id::LOGIN, code) => format!("The login in use can't be read ({code})"),
        (id::PARKED, found::KEPT) => format!("`{subject}`: parked login kept"),
        (id::PARKED, code @ ("vaultKeyMissing" | "keystore")) => {
            format!("`{subject}`: parked login can't be opened ({code}); see the vault key above")
        }
        (id::PARKED, code) => {
            format!("`{subject}`: parked login can't be opened ({code}); sign in to it again")
        }
        (other, code) => format!("{other}: {code}"),
    }
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn main() {
    let cli = Cli::parse();
    let host = match cli.home {
        Some(home) => RealHost::with_home(home),
        None => RealHost::new(),
    };
    let engine = Engine::new(host.clone());

    let result = (|| -> Result<()> {
        match cli.command.unwrap_or(Command::List { offline: false }) {
            Command::List { offline } => {
                let usage = if offline {
                    Vec::new()
                } else {
                    let usage = engine.usage(&Http::new())?;
                    engine.record_plans(&usage);
                    usage
                };
                print_list(&engine.state()?, &usage);
            }
            Command::Enroll {
                name,
                provider,
                sign_in: false,
            } => {
                let provider = ProviderId::parse(&provider)?;
                let enrolled = engine.enroll(provider, &name);
                record(
                    &host,
                    Entry::new(now_epoch(), verb::ENROLL, By::Cli)
                        .provider(provider)
                        .account(&name),
                    &enrolled,
                );
                let meta = enrolled?;
                println!(
                    "Enrolled `{name}` for {} ({}) — now active.",
                    provider.display_name(),
                    meta.label.as_deref().unwrap_or("no email found"),
                );
            }
            Command::Enroll {
                name,
                provider,
                sign_in: true,
            } => {
                let provider = ProviderId::parse(&provider)?;
                // A taken or invalid name fails now, not after a browser sign-in.
                engine.check_new_name(provider, &name)?;
                // Programs live under the real home, whatever --home says.
                let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
                println!(
                    "Signing in to a new {} account. Sign in as the account to add in the browser that opens.",
                    provider.display_name()
                );
                let dir = signin::run_in_terminal(provider, &home)?;
                let login = signin::take_login(engine.host(), provider, dir.path())?;
                let added = engine.add_signed_in(provider, &name, &login);
                record(
                    &host,
                    Entry::new(now_epoch(), verb::SIGN_IN, By::Cli)
                        .provider(provider)
                        .account(&name),
                    &added,
                );
                let meta = added?;
                println!(
                    "Enrolled `{name}` ({}). Switch to it with: piaz-switch use {name}",
                    meta.label.as_deref().unwrap_or("no email found"),
                );
            }
            Command::Use { name, provider } => {
                let provider = parse_provider(provider.as_deref())?;
                // An editor reads its account only when it starts: it is
                // switched while closed.
                let editor = State::load(&host)?
                    .accounts
                    .iter()
                    .filter(|a| a.name == name && provider.is_none_or(|p| p == a.provider))
                    .find_map(|a| pitstop_core::editor::app(a.provider));
                if let Some(app) = editor {
                    // Programs live under the real home, whatever --home says.
                    // No editor installed, nothing reads the login (the
                    // Antigravity 2 app is not Antigravity's editor).
                    let home =
                        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
                    if app.find(&home).is_none() {
                        anyhow::bail!(
                            "Piaz Switch didn't find {} on this computer, so it can't switch it",
                            app.name
                        );
                    }
                    if app.running() {
                        anyhow::bail!(
                            "{0} is open; quit {0}, run this again, then open {0}",
                            app.provider.display_name()
                        );
                    }
                }
                let tool = owner(&engine, &name, provider);
                let from = tool.and_then(|p| in_use(&engine, p));
                let switched = engine.switch(&name, provider);
                let unchanged = matches!(switched, Ok(SwitchOutcome::AlreadyActive));
                if let (Some(p), false) = (tool, unchanged) {
                    record(
                        &host,
                        Entry::new(now_epoch(), verb::USE, By::Cli)
                            .provider(p)
                            .account(&name)
                            .from(from),
                        &switched,
                    );
                }
                match switched? {
                    SwitchOutcome::AlreadyActive => println!("`{name}` is already active."),
                    // `switch` replaces an unmanaged login; only the careful one stops.
                    SwitchOutcome::WouldReplace(_) => unreachable!("switch never stops to ask"),
                    SwitchOutcome::Switched {
                        provider,
                        name,
                        replaced,
                    } => {
                        println!("Switched {} to `{name}`.", provider.display_name());
                        if let Some(replaced) = replaced {
                            println!(
                                "Note: {} was signed in as {}, which Piaz Switch does not manage; that login was replaced, not parked.",
                                provider.display_name(),
                                replaced.label.as_deref().unwrap_or("an unknown account"),
                            );
                        }
                        if provider == ProviderId::Codex {
                            println!(
                                "Note: a running Codex keeps the old account until restarted."
                            );
                        }
                    }
                }
            }
            Command::Rename {
                name,
                new_name,
                provider,
            } => {
                let provider = parse_provider(provider.as_deref())?;
                let renamed = engine.rename(&name, provider, &new_name);
                let mut entry = Entry::new(now_epoch(), verb::RENAME, By::Cli)
                    .account(&new_name)
                    .from(Some(&name));
                if let Ok(meta) = &renamed {
                    entry = entry.provider(meta.provider);
                }
                record(&host, entry, &renamed);
                let meta = renamed?;
                println!(
                    "Renamed `{name}` to `{new_name}` ({}).",
                    meta.label.as_deref().unwrap_or("no email found")
                );
            }
            Command::Remove { name, provider } => {
                let provider = parse_provider(provider.as_deref())?;
                let tool = owner(&engine, &name, provider);
                let removed = engine.remove(&name, provider);
                let mut entry = Entry::new(now_epoch(), verb::FORGET, By::Cli).account(&name);
                if let Some(p) = tool {
                    entry = entry.provider(p);
                }
                record(&host, entry, &removed);
                match removed? {
                    RemoveOutcome::Removed => println!("Removed `{name}`."),
                    RemoveOutcome::RemovedActive => println!(
                        "Removed `{name}` from Piaz Switch. The tool's live login was left untouched."
                    ),
                }
            }
            Command::Log { count } => print_log(&host, count)?,
            Command::Doctor => {
                if !print_doctor(&engine) {
                    std::process::exit(1);
                }
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
