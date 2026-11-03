//! Keeping parked logins fresh while the app is closed: the system runs the
//! `renew` subcommand once a day (Windows Task Scheduler, a macOS
//! LaunchAgent), which renews the parked Claude Code logins whose token has
//! lapsed, as the app does before every read, so one left alone for weeks
//! doesn't expire. While the app runs it does nothing: the app renews them.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::activity::{self, verb, By, Entry};
use crate::engine::Renewal;
use crate::error::{Error, Result};
use crate::presence::{self, Claim};
use crate::usage::UsageApi;
use crate::{Engine, Host, ProviderId};

/// The subcommand the system runs.
pub const SUBCOMMAND: &str = "renew";
/// The job's name in Task Scheduler.
pub const TASK_NAME: &str = "Piaz Switch renew";
/// The LaunchAgent's label, and its file's name.
pub const AGENT_LABEL: &str = "io.github.benpiaz.pitstop.renew";
/// When the job runs, local time. A Mac asleep then runs it on waking; a PC
/// off then waits for the next day, and the app renews whenever it runs.
pub const AT_HOUR: u32 = 13;
pub const AT_MINUTE: u32 = 0;

/// Whether this system has a scheduler Piaz Switch knows.
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// What Task Scheduler runs: the program, quoted, then [`SUBCOMMAND`].
pub fn task_run(program: &Path) -> String {
    format!("\"{}\" {SUBCOMMAND}", program.display())
}

pub fn schtasks_create(program: &Path) -> Vec<String> {
    [
        "/Create",
        "/TN",
        TASK_NAME,
        "/TR",
        &task_run(program),
        "/SC",
        "DAILY",
        "/ST",
        &format!("{AT_HOUR:02}:{AT_MINUTE:02}"),
        "/F",
    ]
    .map(String::from)
    .to_vec()
}

pub fn schtasks_query() -> Vec<String> {
    ["/Query", "/TN", TASK_NAME].map(String::from).to_vec()
}

pub fn schtasks_delete() -> Vec<String> {
    ["/Delete", "/TN", TASK_NAME, "/F"]
        .map(String::from)
        .to_vec()
}

pub fn agent_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist"))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The LaunchAgent: the program with [`SUBCOMMAND`] at [`AT_HOUR`] each day.
pub fn agent_plist(program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>{SUBCOMMAND}</string>
	</array>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>{AT_HOUR}</integer>
		<key>Minute</key>
		<integer>{AT_MINUTE}</integer>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
</dict>
</plist>
"#,
        xml(&program.to_string_lossy())
    )
}

pub fn launchctl_bootstrap(uid: &str, plist: &Path) -> Vec<String> {
    vec![
        "bootstrap".into(),
        format!("gui/{uid}"),
        plist.to_string_lossy().into_owned(),
    ]
}

pub fn launchctl_bootout(uid: &str) -> Vec<String> {
    vec!["bootout".into(), format!("gui/{uid}/{AGENT_LABEL}")]
}

/// A program run for its answer, with no window of its own on Windows.
fn quiet(program: &str, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run it; an error carries what it said on failing.
fn run(program: &str, args: &[String]) -> Result<()> {
    let out = quiet(program, args)
        .output()
        .map_err(|e| Error::InvalidData(format!("{program} didn't start: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim();
    Err(Error::InvalidData(if said.is_empty() {
        format!("{program} failed ({})", out.status)
    } else {
        format!("{program}: {said}")
    }))
}

#[cfg(target_os = "macos")]
fn uid() -> Result<String> {
    let out = quiet("id", &["-u".into()])
        .output()
        .map_err(|e| Error::InvalidData(format!("id didn't start: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Whether the system has the job. `home` is the real home directory.
pub fn installed(home: &Path) -> Result<bool> {
    if cfg!(windows) {
        let out = quiet("schtasks", &schtasks_query())
            .output()
            .map_err(|e| Error::InvalidData(format!("schtasks didn't start: {e}")))?;
        Ok(out.status.success())
    } else if cfg!(target_os = "macos") {
        Ok(agent_path(home).exists())
    } else {
        Ok(false)
    }
}

/// Have the system run `program renew` once a day, replacing any job of
/// ours already there.
pub fn install(home: &Path, program: &Path) -> Result<()> {
    if cfg!(windows) {
        return run("schtasks", &schtasks_create(program));
    }
    #[cfg(target_os = "macos")]
    {
        let plist = agent_path(home);
        if let Some(dir) = plist.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let uid = uid()?;
        // One already loaded is replaced; none loaded is no failure.
        let _ = run("launchctl", &launchctl_bootout(&uid));
        std::fs::write(&plist, agent_plist(program)).map_err(|source| Error::Io {
            path: plist.clone(),
            source,
        })?;
        return run("launchctl", &launchctl_bootstrap(&uid, &plist));
    }
    #[allow(unreachable_code)]
    {
        let _ = (home, program);
        Err(Error::InvalidData(
            "this system has no scheduler Piaz Switch knows".into(),
        ))
    }
}

/// Take the job away; none there is no failure.
pub fn uninstall(home: &Path) -> Result<()> {
    if cfg!(windows) {
        if !installed(home)? {
            return Ok(());
        }
        return run("schtasks", &schtasks_delete());
    }
    #[cfg(target_os = "macos")]
    {
        let plist = agent_path(home);
        let _ = run("launchctl", &launchctl_bootout(&uid()?));
        if plist.exists() {
            std::fs::remove_file(&plist).map_err(|source| Error::Io {
                path: plist,
                source,
            })?;
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        let _ = home;
        Ok(())
    }
}

/// What a renewal came to, as the activity log records it.
pub fn renewal_result(renewal: &Renewal) -> &'static str {
    match renewal {
        Renewal::Renewed => activity::DONE,
        Renewal::Refused => "refused",
        Renewal::Failed(_) => "failed",
    }
}

/// Whether an outcome is news: a renewal that worked always is; a refusal
/// or failure only when the log's last word on that account differs, so a
/// login refused day after day is logged once.
pub fn worth_logging(log: &[Entry], name: &str, result: &str) -> bool {
    let last = log
        .iter()
        .find(|e| e.verb == verb::RENEW && e.account.as_deref() == Some(name))
        .map(|e| e.result.as_str());
    result == activity::DONE || last != Some(result)
}

/// What a `renew` run came to.
#[derive(Debug)]
pub enum RenewRun {
    /// The app is open, and renews them itself.
    AppOpen,
    /// Whether the app is open couldn't be told, so nothing was renewed.
    Unsure,
    /// The parked logins that were due, and what came of each.
    Ran(Vec<(String, Renewal)>),
}

/// Renew the parked Claude Code logins that are due, unless the app is
/// open; each outcome goes in the activity log as asked `by`. The app holds
/// the presence lock while it runs; `app_running` says whether one that
/// holds none runs anyway (a version from before the lock), as
/// [`presence::app_process_running`] does.
pub fn renew_now<H: Host>(
    engine: &Engine<H>,
    api: &dyn UsageApi,
    by: By,
    now: i64,
    app_running: &dyn Fn() -> bool,
) -> Result<RenewRun> {
    let _held = match presence::claim(&engine.host().home()) {
        Claim::Held(held) => held,
        Claim::Busy => return Ok(RenewRun::AppOpen),
        Claim::Unknown => return Ok(RenewRun::Unsure),
    };
    if app_running() {
        return Ok(RenewRun::AppOpen);
    }
    // Nothing to renew asks nothing of the keychain.
    let any = engine
        .state()?
        .accounts
        .iter()
        .any(|a| a.provider == ProviderId::ClaudeCode);
    if !any {
        return Ok(RenewRun::Ran(Vec::new()));
    }
    let renewed = engine.renew_parked(api, &|_| true)?;
    let log = activity::recent(engine.host(), activity::KEEP).unwrap_or_default();
    for (name, renewal) in &renewed {
        let result = renewal_result(renewal);
        if worth_logging(&log, name, result) {
            let _ = activity::record(
                engine.host(),
                &Entry::new(now, verb::RENEW, by)
                    .provider(ProviderId::ClaudeCode)
                    .account(name)
                    .result(result),
            );
        }
    }
    Ok(RenewRun::Ran(renewed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RealHost;

    #[test]
    fn task_scheduler_runs_the_program_quoted_once_a_day() {
        let program = Path::new(r"C:\Users\ben\AppData\Local\Piaz Switch\pitstop-app.exe");
        let args = schtasks_create(program);
        assert_eq!(args[..3], ["/Create", "/TN", TASK_NAME]);
        let tr = args.iter().position(|a| a == "/TR").unwrap();
        assert_eq!(
            args[tr + 1],
            r#""C:\Users\ben\AppData\Local\Piaz Switch\pitstop-app.exe" renew"#
        );
        let sc = args.iter().position(|a| a == "/SC").unwrap();
        assert_eq!(args[sc + 1], "DAILY");
        assert_eq!(args[sc + 3], "13:00");
        assert_eq!(args.last().unwrap(), "/F");
        assert_eq!(schtasks_query(), ["/Query", "/TN", TASK_NAME]);
        assert_eq!(schtasks_delete(), ["/Delete", "/TN", TASK_NAME, "/F"]);
    }

    #[test]
    fn the_launch_agent_runs_the_program_at_one_each_day() {
        let program = Path::new("/Applications/Piaz Switch.app/Contents/MacOS/pitstop-app");
        let plist = agent_plist(program);
        assert!(plist.contains(&format!("<string>{AGENT_LABEL}</string>")));
        assert!(plist
            .contains("<string>/Applications/Piaz Switch.app/Contents/MacOS/pitstop-app</string>"));
        assert!(plist.contains("<string>renew</string>"));
        assert!(plist.contains("<key>Hour</key>\n\t\t<integer>13</integer>"));
        assert!(agent_plist(Path::new("/a&b<c")).contains("/a&amp;b&lt;c"));
        assert!(agent_path(Path::new("/Users/ben"))
            .ends_with(format!("Library/LaunchAgents/{AGENT_LABEL}.plist")));
        assert_eq!(
            launchctl_bootstrap("501", Path::new("/p.plist")),
            ["bootstrap", "gui/501", "/p.plist"]
        );
        assert_eq!(
            launchctl_bootout("501"),
            ["bootout".to_string(), format!("gui/501/{AGENT_LABEL}")]
        );
    }

    #[test]
    fn a_refusal_repeated_day_after_day_is_logged_once() {
        let refused = |at| {
            Entry::new(at, verb::RENEW, By::Cli)
                .account("ben3")
                .result("refused")
        };
        // Newest first.
        let log = vec![
            refused(20),
            Entry::new(10, verb::RENEW, By::Cli).account("ben3"),
        ];
        assert!(!worth_logging(&log, "ben3", "refused"));
        assert!(worth_logging(&log, "ben3", "failed"));
        assert!(worth_logging(&log, "ben3", activity::DONE));
        assert!(worth_logging(&log, "ben2", "refused"));
        assert!(worth_logging(&[], "ben3", "refused"));
    }

    struct NoNetwork;

    impl UsageApi for NoNetwork {
        fn claude(
            &self,
            _: &str,
        ) -> std::result::Result<serde_json::Value, crate::usage::FetchError> {
            unreachable!()
        }
        fn codex(
            &self,
            _: &str,
            _: &str,
        ) -> std::result::Result<serde_json::Value, crate::usage::FetchError> {
            unreachable!()
        }
        fn cursor(
            &self,
            _: &str,
        ) -> std::result::Result<serde_json::Value, crate::usage::FetchError> {
            unreachable!()
        }
        fn renew_claude(
            &self,
            _: &crate::usage::ClaudeRenewal,
        ) -> std::result::Result<serde_json::Value, crate::usage::FetchError> {
            unreachable!()
        }
    }

    #[test]
    fn nothing_is_renewed_while_the_app_is_open() {
        let home = tempfile::tempdir().unwrap();
        let engine = Engine::new(RealHost::with_home(home.path().to_path_buf()));
        let app = presence::claim(home.path());
        assert!(matches!(app, Claim::Held(_)));
        let run = renew_now(&engine, &NoNetwork, By::Cli, 0, &|| false).unwrap();
        assert!(matches!(run, RenewRun::AppOpen));
        drop(app);
        // An older app, holding no lock, still counts as open.
        let run = renew_now(&engine, &NoNetwork, By::Cli, 0, &|| true).unwrap();
        assert!(matches!(run, RenewRun::AppOpen));
        // Closed, with no Claude Code account: a run that asks nobody.
        let run = renew_now(&engine, &NoNetwork, By::Cli, 0, &|| false).unwrap();
        assert!(matches!(run, RenewRun::Ran(renewed) if renewed.is_empty()));
    }
}
