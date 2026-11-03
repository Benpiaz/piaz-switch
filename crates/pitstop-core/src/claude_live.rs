//! The 5-hour and weekly limits Claude Code hands to a status line command
//! (`rate_limits` in its stdin JSON, Claude Code 2.1.80 and later, for claude.ai
//! subscribers, after the first reply of a session).
//!
//! The usage endpoint turns reads away often (a limit of a few requests per
//! access token), while these figures cost no request at all. A status line
//! command saves them here; a refused or expired read of the account whose
//! login is in use shows them instead. They cover that one account and only
//! the two overall limits, so per-model limits still come from the endpoint.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::provider::{provider_for, Details, ProviderId};
use crate::usage::{Usage, Window, FIVE_HOURS, WEEK};

const FILE: &str = "claude-live-limits.json";

/// The argument that makes the app act as the status line command.
pub const STATUS_LINE_FLAG: &str = "--statusline";

/// How long a saved reading stands in for a read the service turned away.
pub const FRESH_FOR: i64 = 15 * 60;

fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join(FILE)
}

/// Save the `rate_limits` of one status line input, for the login in use now.
/// An input without them (not a subscriber, or before the first reply) saves
/// nothing, and neither does one that is not JSON.
pub fn save(host: &dyn Host, input: &str, now: i64) -> Result<()> {
    let Ok(input) = serde_json::from_str::<Value>(input) else {
        return Ok(());
    };
    let Some(limits) = input.get("rate_limits").filter(|l| l.is_object()) else {
        return Ok(());
    };
    let identity = provider_for(ProviderId::ClaudeCode)
        .read_active(host)?
        .and_then(|login| login.details.identity);
    let saved = json!({ "saved_at": now, "identity": identity, "rate_limits": limits });
    let bytes = serde_json::to_vec(&saved).map_err(|e| Error::InvalidData(e.to_string()))?;
    host.write_atomic(&path(host), &bytes)
}

/// The saved limits, when they were read recently for this very login.
pub fn load(host: &dyn Host, login: &Details, now: i64) -> Option<Usage> {
    let saved: Value = serde_json::from_slice(&host.read(&path(host)).ok()??).ok()?;
    let saved_at = saved.get("saved_at")?.as_i64()?;
    if now - saved_at > FRESH_FOR || saved_at - now > 60 {
        return None;
    }
    let identity = saved.get("identity")?.as_str()?;
    if login.identity.as_deref() != Some(identity) {
        return None;
    }
    let limits = saved.get("rate_limits")?;
    let windows: Vec<Window> = [("five_hour", "5h", FIVE_HOURS), ("seven_day", "week", WEEK)]
        .into_iter()
        .filter_map(|(key, label, length)| window(limits.get(key)?, label, length))
        .collect();
    (!windows.is_empty()).then_some(Usage {
        balance: None,
        windows,
        plan: None,
    })
}

fn window(value: &Value, label: &str, length: i64) -> Option<Window> {
    let percent = value.get("used_percentage")?.as_f64()?;
    if !percent.is_finite() || percent < 0.0 {
        return None;
    }
    let resets_at = value.get("resets_at").and_then(Value::as_i64);
    Some(Window {
        label: label.into(),
        percent,
        resets_at,
        length_secs: Some(length),
        quiet: false,
        idle: percent == 0.0 && resets_at.is_none(),
    })
}

fn settings_path(host: &dyn Host) -> PathBuf {
    host.home().join(".claude").join("settings.json")
}

/// Whether Claude Code's status line is this app's.
pub fn status_line_installed(host: &dyn Host) -> bool {
    host.read(&settings_path(host))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|settings| {
            settings
                .pointer("/statusLine/command")?
                .as_str()
                .map(|command| command.contains(STATUS_LINE_FLAG))
        })
        .unwrap_or(false)
}

/// The `statusLine` entry of Claude Code's settings that runs `command`.
fn status_line_entry(command: &str) -> String {
    let command = serde_json::to_string(command).unwrap_or_default();
    format!("\"statusLine\": {{ \"type\": \"command\", \"command\": {command} }}")
}

/// Add a `statusLine` setting running `command` to Claude Code's
/// `settings.json`, keeping every other byte of the file as it is. A status
/// line the user already set is theirs: nothing changes and `false` comes
/// back.
pub fn install_status_line(host: &dyn Host, command: &str) -> Result<bool> {
    let file = settings_path(host);
    let text = match host.read(&file)? {
        Some(bytes) => String::from_utf8(bytes)
            .map_err(|_| Error::InvalidData("settings.json is not UTF-8".into()))?,
        None => "{}".to_owned(),
    };
    let settings: Value = serde_json::from_str(&text)
        .map_err(|e| Error::InvalidData(format!("settings.json: {e}")))?;
    let object = settings
        .as_object()
        .ok_or_else(|| Error::InvalidData("settings.json is not an object".into()))?;
    if object.contains_key("statusLine") {
        return Ok(false);
    }
    let close = text
        .rfind('}')
        .ok_or_else(|| Error::InvalidData("settings.json has no closing brace".into()))?;
    let (head, tail) = text.split_at(close);
    let separator = if object.is_empty() { "" } else { "," };
    let updated = format!(
        "{}{separator}\n  {}\n{tail}",
        head.trim_end(),
        status_line_entry(command)
    );
    host.write_atomic(&file, updated.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::claude_code::ClaudeCode;
    use crate::testing::FakeHost;

    const NOW: i64 = 1_800_000_000;

    fn signed_in(host: &FakeHost, email: &str) {
        host.put_file(
            ClaudeCode::credentials_path(host),
            r#"{"claudeAiOauth":{"accessToken":"T","refreshToken":"R"}}"#,
        );
        host.put_file(
            ClaudeCode::config_path(host),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"uuid-{email}","organizationUuid":"org"}}}}"#
            ),
        );
    }

    fn login(host: &FakeHost) -> Details {
        provider_for(ProviderId::ClaudeCode)
            .read_active(host)
            .unwrap()
            .unwrap()
            .details
    }

    const INPUT: &str = r#"{"model":{"display_name":"Opus"},"rate_limits":{
        "five_hour":{"used_percentage":10.5,"resets_at":1800003600},
        "seven_day":{"used_percentage":41,"resets_at":1800400000}}}"#;

    #[test]
    fn the_limits_of_a_status_line_input_come_back_for_the_same_login() {
        let host = FakeHost::new();
        signed_in(&host, "a@x.com");
        save(&host, INPUT, NOW).unwrap();
        let usage = load(&host, &login(&host), NOW + 60).unwrap();
        let shown: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.percent, w.resets_at, w.length_secs))
            .collect();
        assert_eq!(
            shown,
            [
                ("5h", 10.5, Some(1_800_003_600), Some(FIVE_HOURS)),
                ("week", 41.0, Some(1_800_400_000), Some(WEEK)),
            ]
        );
    }

    #[test]
    fn another_login_or_an_old_reading_gets_nothing() {
        let host = FakeHost::new();
        signed_in(&host, "a@x.com");
        save(&host, INPUT, NOW).unwrap();
        assert!(load(&host, &login(&host), NOW + FRESH_FOR + 1).is_none());
        signed_in(&host, "b@x.com");
        assert!(load(&host, &login(&host), NOW + 60).is_none());
    }

    #[test]
    fn an_input_without_limits_saves_nothing() {
        let host = FakeHost::new();
        signed_in(&host, "a@x.com");
        save(&host, r#"{"model":{"display_name":"Opus"}}"#, NOW).unwrap();
        save(&host, "not json", NOW).unwrap();
        assert!(load(&host, &login(&host), NOW).is_none());
    }

    #[test]
    fn a_status_line_is_added_beside_the_other_settings_and_never_replaced() {
        let host = FakeHost::new();
        let file = host.home().join(".claude").join("settings.json");
        host.put_file(&file, "{\n  \"model\": \"opus\",\n  \"env\": {}\n}\n");
        assert!(!status_line_installed(&host));
        assert!(install_status_line(&host, r"C:\P S\app.exe --statusline").unwrap());
        assert!(status_line_installed(&host));
        let text = String::from_utf8(host.file(&file).unwrap()).unwrap();
        assert!(text.starts_with("{\n  \"model\": \"opus\",\n  \"env\": {},\n  \"statusLine\""));
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["statusLine"]["command"],
            r"C:\P S\app.exe --statusline"
        );
        assert_eq!(parsed["env"], json!({}));
        assert!(!install_status_line(&host, "other").unwrap());
        assert_eq!(String::from_utf8(host.file(&file).unwrap()).unwrap(), text);
    }

    #[test]
    fn a_missing_settings_file_gets_a_status_line_too() {
        let host = FakeHost::new();
        assert!(install_status_line(&host, "x").unwrap());
        let file = host.home().join(".claude").join("settings.json");
        let parsed: Value = serde_json::from_slice(&host.file(&file).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["type"], "command");
    }
}
