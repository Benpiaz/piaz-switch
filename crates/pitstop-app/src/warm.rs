//! A warm-up: Claude Code's own print mode, run once for an account in a
//! directory of its own, so the request starts that account's next five-hour
//! period. The account in use lends only its access token, so the login Claude
//! Code uses is never touched; a parked account lends its whole login, which
//! Claude Code renews as it runs when its token has lapsed, and the renewed
//! login goes back in the vault. What was tried is kept in
//! `~/.pitstop/warmed.json`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pitstop_core::engine::vault_id;
use pitstop_core::provider::claude_code::ClaudeCode;
use pitstop_core::provider::{provider_for, Provider};
use pitstop_core::{vault, Host, ProviderId};
use serde_json::Value;

use crate::duty::Warmed;

const PROMPT: &str = "Reply with the single word OK.";
const TIMEOUT: Duration = Duration::from_secs(120);

/// Why a warm-up came to nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The limit is used up: nothing starts until it resets.
    LimitReached,
    /// The service refused the login.
    LoginRefused,
    /// The service was out of reach or overloaded.
    Unreachable,
    /// Anything else, in Claude Code's words.
    Failed(String),
}

impl Failure {
    /// A stable name for what it came to, kept in `warmed.json` and shown in Settings.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::LimitReached => "limitReached",
            Failure::LoginRefused => "loginRefused",
            Failure::Unreachable => "unreachable",
            Failure::Failed(_) => "failed",
        }
    }
}

/// An account's access token, when it lapses (epoch seconds) and whether the
/// login holds what renews it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub access: String,
    pub expires_at: Option<i64>,
    pub renewable: bool,
}

impl Token {
    pub fn lapsed(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|until| until <= now)
    }
}

/// The token in a Claude Code login: a vault blob, whose `credentials` hold
/// the `claudeAiOauth` Claude Code keeps on disk or in the keychain.
pub fn token_of(blob: &[u8]) -> Option<Token> {
    let blob: Value = serde_json::from_slice(blob).ok()?;
    let login: Value = serde_json::from_str(blob.get("credentials")?.as_str()?).ok()?;
    let oauth = login.get("claudeAiOauth")?;
    let access = oauth.get("accessToken")?.as_str()?.to_owned();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(Value::as_i64)
        .map(|ms| ms / 1000);
    let renewable = oauth
        .get("refreshToken")
        .and_then(Value::as_str)
        .is_some_and(|r| !r.is_empty());
    Some(Token {
        access,
        expires_at,
        renewable,
    })
}

/// The token of `name`: the login Claude Code holds when it is the account in
/// use, its parked copy otherwise.
pub fn token_for(host: &dyn Host, name: &str, in_use: bool) -> Option<Token> {
    let blob = if in_use {
        provider_for(ProviderId::ClaudeCode)
            .read_active(host)
            .ok()??
            .blob
    } else {
        vault::load(host, &vault_id(ProviderId::ClaudeCode, name)).ok()?
    };
    token_of(&blob)
}

/// Print mode on the smallest model, with no tools, MCP servers, slash
/// commands or kept session, answering in JSON. `dir` is its config home;
/// without `token` it signs in with the login lent to `dir`.
pub fn command(program: &Path, dir: &Path, token: Option<&str>, search: &[PathBuf]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args([
        "-p",
        PROMPT,
        "--model",
        "haiku",
        "--tools",
        "",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--output-format",
        "json",
    ])
    .current_dir(dir)
    .env("CLAUDE_CONFIG_DIR", dir)
    .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
    .env("DISABLE_AUTOUPDATER", "1")
    .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
    .env_remove("ANTHROPIC_API_KEY")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    match token {
        Some(token) => cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token),
        None => cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN"),
    };
    if let Ok(path) = std::env::join_paths(search) {
        cmd.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// What print mode's JSON result came to: `is_error` false and a clean exit
/// is a warm-up; a 401/403 a refused login; 429 `usage_limit_reached` a used
/// up limit; another 429 or a 5xx a service out of reach.
pub fn verdict(exit: Option<i32>, stdout: &str) -> Result<(), Failure> {
    let Some(result) = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(Value::is_object)
    else {
        return Err(Failure::Failed("Claude Code printed no result".into()));
    };
    if result.get("is_error").and_then(Value::as_bool) == Some(false) && exit == Some(0) {
        return Ok(());
    }
    let said = result
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("Claude Code failed")
        .to_owned();
    match (
        result.get("api_error_status").and_then(Value::as_u64),
        result.get("api_error").and_then(Value::as_str),
    ) {
        (Some(429), Some("usage_limit_reached")) => Err(Failure::LimitReached),
        (Some(401 | 403), _) => Err(Failure::LoginRefused),
        (Some(429 | 500..=599), _) => Err(Failure::Unreachable),
        _ => Err(Failure::Failed(said)),
    }
}

fn new_dir() -> Result<PathBuf, Failure> {
    let dir = std::env::temp_dir().join(format!("piaz-warm-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir).map_err(|e| Failure::Failed(e.to_string()))?;
    Ok(dir)
}

/// Run one warm-up with `token`, in a directory made for it and removed after.
pub fn run(program: &Path, token: &str, search: &[PathBuf]) -> Result<(), Failure> {
    let dir = new_dir()?;
    let outcome = run_in(program, &dir, Some(token), search);
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// Run one warm-up of the parked account `name` with its whole login, lent to
/// a directory made for it; a login Claude Code renewed meanwhile goes back in
/// the vault, whatever the warm-up came to. The caller keeps switches out
/// while this runs: a switch to `name` would put in use a login this renews.
pub fn run_parked(
    host: &dyn Host,
    name: &str,
    program: &Path,
    search: &[PathBuf],
) -> Result<(), Failure> {
    let id = vault_id(ProviderId::ClaudeCode, name);
    let blob = vault::load(host, &id).map_err(|e| Failure::Failed(e.to_string()))?;
    let dir = new_dir()?;
    let claude = ClaudeCode::for_this_os();
    let mut outcome = claude
        .lend(host, &dir, &blob)
        .map_err(|e| Failure::Failed(e.to_string()))
        .and_then(|()| run_in(program, &dir, None, search));
    if let Ok(Some(renewed)) = claude.renewed(host, &dir, &blob) {
        if let Err(e) = vault::store(host, &id, &renewed) {
            outcome = Err(Failure::Failed(format!(
                "couldn't keep the renewed login: {e}"
            )));
        }
    }
    let _ = claude.discard_signed_in(host, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn run_in(
    program: &Path,
    dir: &Path,
    token: Option<&str>,
    search: &[PathBuf],
) -> Result<(), Failure> {
    let mut child = command(program, dir, token, search)
        .spawn()
        .map_err(|e| Failure::Failed(format!("couldn't start Claude Code: {e}")))?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut text);
        }
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(Failure::Failed(e.to_string())),
        }
    };
    let said = reader.join().unwrap_or_default();
    match status {
        None => Err(Failure::Failed("Claude Code took too long".into())),
        Some(status) => verdict(status.code(), &said),
    }
}

// ---------- what was tried ----------

/// Every read-modify-write of `warmed.json` in this process holds this, so a
/// rename or a forget never interleaves with writing down a warm-up.
static LEDGER: Mutex<()> = Mutex::new(());

/// Every account's [`Warmed`], by name, in `~/.pitstop/warmed.json`.
pub type Ledger = BTreeMap<String, Warmed>;

fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("warmed.json")
}

/// The ledger, with failures counted afresh on a day after the one they were
/// counted for. A missing or unreadable file is an empty ledger.
pub fn load(host: &dyn Host, today: &str) -> Ledger {
    let mut ledger = read(host);
    for warmed in ledger.values_mut() {
        new_day(warmed, today);
    }
    ledger
}

fn read(host: &dyn Host) -> Ledger {
    host.read(&path(host))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(host: &dyn Host, ledger: &Ledger) {
    let bytes = serde_json::to_vec_pretty(ledger).unwrap_or_default();
    let _ = host.write_atomic(&path(host), &bytes);
}

fn new_day(warmed: &mut Warmed, today: &str) {
    if warmed.date != today {
        warmed.date = today.to_owned();
        warmed.failures = 0;
        warmed.last_failure = None;
        warmed.last_error = None;
    }
}

/// Write down a warm-up of `name` at `now`: one that worked marks the start
/// of its period, one that failed counts against today's tries.
pub fn record(
    ledger: &mut Ledger,
    name: &str,
    today: &str,
    now: i64,
    outcome: Result<(), &Failure>,
) {
    let warmed = ledger.entry(name.to_owned()).or_default();
    new_day(warmed, today);
    match outcome {
        Ok(()) => {
            warmed.started = Some(now);
            warmed.last_error = None;
        }
        Err(failure) => {
            warmed.failures += 1;
            warmed.last_failure = Some(now);
            warmed.last_error = Some(failure.code().to_owned());
        }
    }
}

/// An account renamed keeps what was tried for it.
pub fn rename_in(ledger: &mut Ledger, old: &str, new: &str) {
    if let Some(warmed) = ledger.remove(old) {
        ledger.insert(new.to_owned(), warmed);
    }
}

pub fn forget_in(ledger: &mut Ledger, name: &str) {
    ledger.remove(name);
}

/// Write down a warm-up of `name` that just ended, on the ledger as it is now
/// on disk, so what changed while it ran stays changed. Nothing is written
/// when `still_enrolled` says the account went (renamed or forgotten) in the
/// meantime; it is asked under the same lock a rename or forget takes.
pub fn update_if(
    host: &dyn Host,
    name: &str,
    today: &str,
    now: i64,
    outcome: Result<(), &Failure>,
    still_enrolled: impl FnOnce() -> bool,
) -> Warmed {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = load(host, today);
    if still_enrolled() {
        record(&mut ledger, name, today, now, outcome);
        save(host, &ledger);
    }
    ledger.get(name).cloned().unwrap_or_default()
}

/// Carry an account's record over a rename, in `warmed.json` itself.
pub fn rename(host: &dyn Host, old: &str, new: &str) {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = read(host);
    if ledger.contains_key(old) {
        rename_in(&mut ledger, old, new);
        save(host, &ledger);
    }
}

pub fn forget(host: &dyn Host, name: &str) {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = read(host);
    if ledger.contains_key(name) {
        forget_in(&mut ledger, name);
        save(host, &ledger);
    }
}

/// What a Claude Code account's rename carries: its warm-up setting and its
/// record both move to the new name.
pub fn follow_rename(host: &dyn Host, old: &str, new: &str) {
    let mut saved = crate::settings::load(host);
    saved.rename_account(old, new);
    let _ = crate::settings::save(host, &saved);
    rename(host, old, new);
}

/// What forgetting a Claude Code account takes with it.
pub fn follow_forget(host: &dyn Host, name: &str) {
    let mut saved = crate::settings::load(host);
    saved.forget_account(name);
    let _ = crate::settings::save(host, &saved);
    forget(host, name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_command_runs_print_mode_with_the_token_in_its_own_home_and_nothing_else_of_ours() {
        let cmd = command(
            Path::new("claude"),
            Path::new("/tmp/w"),
            Some("tok-123"),
            &[],
        );
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args[0], OsStr::new("-p"));
        assert_eq!(args[1], OsStr::new(PROMPT));
        for flag in [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--disable-slash-commands",
        ] {
            assert!(args.contains(&OsStr::new(flag)), "{flag}");
        }
        let at = args
            .iter()
            .position(|a| *a == OsStr::new("--tools"))
            .unwrap();
        assert_eq!(args[at + 1], OsStr::new(""));
        let env: Vec<_> = cmd.get_envs().collect();
        let value = |k: &str| {
            env.iter()
                .find(|(key, _)| *key == OsStr::new(k))
                .map(|(_, v)| *v)
        };
        assert_eq!(value("CLAUDE_CONFIG_DIR"), Some(Some(OsStr::new("/tmp/w"))));
        assert_eq!(
            value("CLAUDE_CODE_OAUTH_TOKEN"),
            Some(Some(OsStr::new("tok-123")))
        );
        assert_eq!(value("ANTHROPIC_API_KEY"), Some(None));
        assert_eq!(value("CLAUDE_SECURESTORAGE_CONFIG_DIR"), Some(None));
    }

    #[test]
    fn without_a_token_the_command_signs_in_with_the_lent_login_alone() {
        let cmd = command(Path::new("claude"), Path::new("/tmp/w"), None, &[]);
        let token = cmd
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("CLAUDE_CODE_OAUTH_TOKEN"))
            .map(|(_, v)| v);
        assert_eq!(token, Some(None));
    }

    #[test]
    fn each_result_print_mode_gives_is_read_as_what_it_came_to() {
        let cases = [
            (Some(0), r#"{"is_error":false,"result":"OK"}"#, Ok(())),
            (
                Some(0),
                "warning first\n{\"is_error\":false,\"result\":\"OK\"}\n",
                Ok(()),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":429,"api_error":"usage_limit_reached","result":"x"}"#,
                Err(Failure::LimitReached),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":429,"api_error":"rate_limit_error"}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":529}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":401,"result":"bad"}"#,
                Err(Failure::LoginRefused),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":403,"result":"forbidden"}"#,
                Err(Failure::LoginRefused),
            ),
            (
                Some(1),
                r#"{"is_error":true,"result":"something else"}"#,
                Err(Failure::Failed("something else".into())),
            ),
            (
                Some(1),
                r#"{"is_error":false,"result":"OK"}"#,
                Err(Failure::Failed("OK".into())),
            ),
            (
                Some(0),
                "",
                Err(Failure::Failed("Claude Code printed no result".into())),
            ),
        ];
        for (exit, stdout, expected) in cases {
            assert_eq!(verdict(exit, stdout), expected, "{stdout}");
        }
    }

    #[test]
    fn a_vault_blob_gives_its_token_when_it_lapses_and_whether_it_renews() {
        let blob = |oauth: &str| {
            serde_json::to_vec(&serde_json::json!({
                "credentials": format!(r#"{{"claudeAiOauth":{oauth}}}"#),
                "oauth_account": {"emailAddress": "a@x.com"},
            }))
            .unwrap()
        };
        let token = token_of(&blob(
            r#"{"accessToken":"a-1","refreshToken":"r-1","expiresAt":1800000000123}"#,
        ))
        .unwrap();
        assert_eq!(
            token,
            Token {
                access: "a-1".into(),
                expires_at: Some(1_800_000_000),
                renewable: true,
            }
        );
        assert!(!token.lapsed(1_799_999_999));
        assert!(token.lapsed(1_800_000_000));
        assert!(
            !token_of(&blob(r#"{"accessToken":"a-1"}"#))
                .unwrap()
                .renewable
        );
        assert_eq!(token_of(br#"{"other":1}"#), None);
        // what Claude Code itself keeps is not a vault blob
        assert_eq!(
            token_of(br#"{"claudeAiOauth":{"accessToken":"a-1"}}"#),
            None
        );
    }

    #[test]
    fn failures_count_per_day_and_a_success_marks_the_start() {
        let mut ledger = Ledger::new();
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            100,
            Err(&Failure::Unreachable),
        );
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            200,
            Err(&Failure::Unreachable),
        );
        assert_eq!(ledger["ben2"].failures, 2);
        assert_eq!(ledger["ben2"].last_failure, Some(200));
        record(&mut ledger, "ben2", "2026-10-16", 300, Ok(()));
        assert_eq!(ledger["ben2"].failures, 0);
        assert_eq!(ledger["ben2"].last_failure, None);
        assert_eq!(ledger["ben2"].started, Some(300));
    }

    #[test]
    fn a_failure_is_written_down_with_its_kind_and_a_success_clears_it() {
        let mut ledger = Ledger::new();
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            100,
            Err(&Failure::LoginRefused),
        );
        assert_eq!(ledger["ben2"].last_error.as_deref(), Some("loginRefused"));
        record(&mut ledger, "ben2", "2026-10-15", 200, Ok(()));
        assert_eq!(ledger["ben2"].last_error, None);
    }

    #[test]
    fn a_renamed_account_keeps_its_record_and_a_forgotten_one_loses_it() {
        let mut ledger = Ledger::new();
        record(&mut ledger, "old", "2026-10-15", 100, Ok(()));
        rename_in(&mut ledger, "old", "new");
        assert!(!ledger.contains_key("old"));
        assert_eq!(ledger["new"].started, Some(100));
        forget_in(&mut ledger, "new");
        assert!(ledger.is_empty());
    }

    #[test]
    fn writing_down_a_warm_up_keeps_what_changed_on_disk_meanwhile() {
        let host = pitstop_core::testing::FakeHost::new();
        let mut ledger = Ledger::new();
        record(&mut ledger, "a", "2026-10-15", 1, Ok(()));
        record(&mut ledger, "b", "2026-10-15", 1, Ok(()));
        save(&host, &ledger);
        // While a's warm-up runs, b is forgotten.
        forget(&host, "b");
        let a = update_if(
            &host,
            "a",
            "2026-10-15",
            50,
            Err(&Failure::Unreachable),
            || true,
        );
        assert_eq!(a.failures, 1);
        let on_disk = load(&host, "2026-10-15");
        assert!(!on_disk.contains_key("b"), "a forgotten account came back");
        assert_eq!(on_disk["a"].last_failure, Some(50));
    }

    #[test]
    fn a_rename_and_a_forget_carry_the_setting_and_the_record() {
        let host = pitstop_core::testing::FakeHost::new();
        let settings = crate::settings::Settings {
            warm_up: vec!["old".into(), "other".into()],
            ..Default::default()
        };
        crate::settings::save(&host, &settings).unwrap();
        let mut ledger = Ledger::new();
        record(&mut ledger, "old", "2026-10-15", 7, Ok(()));
        save(&host, &ledger);

        follow_rename(&host, "old", "new");
        assert_eq!(
            crate::settings::load(&host).warm_up,
            vec!["new".to_string(), "other".to_string()]
        );
        assert_eq!(load(&host, "2026-10-15")["new"].started, Some(7));

        follow_forget(&host, "new");
        assert_eq!(
            crate::settings::load(&host).warm_up,
            vec!["other".to_string()]
        );
        assert!(load(&host, "2026-10-15").is_empty());
    }

    #[test]
    fn a_warm_up_of_an_account_that_went_meanwhile_is_not_written_down() {
        let host = pitstop_core::testing::FakeHost::new();
        update_if(&host, "gone", "2026-10-15", 9, Ok(()), || false);
        assert!(load(&host, "2026-10-15").is_empty());
    }
}
