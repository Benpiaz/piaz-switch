use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pitstop_core::engine::{AccountUsage, RemoveOutcome, SwitchOutcome};
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
                let meta = engine.enroll(provider, &name)?;
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
                let meta = engine.add_signed_in(provider, &name, &login)?;
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
                match engine.switch(&name, provider)? {
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
                let meta = engine.rename(&name, parse_provider(provider.as_deref())?, &new_name)?;
                println!(
                    "Renamed `{name}` to `{new_name}` ({}).",
                    meta.label.as_deref().unwrap_or("no email found")
                );
            }
            Command::Remove { name, provider } => {
                match engine.remove(&name, parse_provider(provider.as_deref())?)? {
                    RemoveOutcome::Removed => println!("Removed `{name}`."),
                    RemoveOutcome::RemovedActive => println!(
                        "Removed `{name}` from Piaz Switch. The tool's live login was left untouched."
                    ),
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
