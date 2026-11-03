//! Claude Code's `statusLine` in `~/.claude/settings.json`: added only where
//! there is none, removed only when it is Piaz Switch's own. Every other
//! setting stays as it was, in its order.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::Host;

/// The subcommand Claude Code runs.
pub const SUBCOMMAND: &str = "statusline";

pub fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".claude").join("settings.json")
}

/// What Claude Code's settings hold for its status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLine {
    /// None yet.
    Absent,
    /// Piaz Switch's, running this command.
    Ours(String),
    /// Someone else's, running this command (or another kind of line).
    Other(String),
    /// The settings can't be read as JSON, so nothing is changed.
    Unreadable,
}

/// The command Claude Code runs for the line: the program, quoted for the
/// shell Claude Code runs it in, then [`SUBCOMMAND`]. Forward slashes on
/// Windows read the same in Git Bash and in cmd.
pub fn command_for(program: &Path) -> String {
    let mut path = program.to_string_lossy().into_owned();
    if cfg!(windows) {
        path = path.replace('\\', "/");
    }
    let quoted: String = path
        .chars()
        .flat_map(|c| match c {
            '"' | '\\' | '$' | '`' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!("\"{quoted}\" {SUBCOMMAND}")
}

/// Whether `command` is a status line Piaz Switch added: its own program
/// running [`SUBCOMMAND`], wherever the program was then.
pub fn is_ours(command: &str) -> bool {
    let command = command.trim();
    command.ends_with(&format!(" {SUBCOMMAND}"))
        && ["pitstop-app", "piaz-switch", "Piaz Switch"]
            .iter()
            .any(|name| command.contains(name))
}

/// Settings as an object; none when they are empty or missing, an error
/// when they are anything but a JSON object.
fn parse(bytes: Option<&[u8]>) -> std::result::Result<Map<String, Value>, ()> {
    let Some(bytes) = bytes.filter(|b| !b.iter().all(u8::is_ascii_whitespace)) else {
        return Ok(Map::new());
    };
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(()),
    }
}

/// What `bytes`, the settings file (none when missing), says of the line.
pub fn status_line(bytes: Option<&[u8]>) -> StatusLine {
    let Ok(settings) = parse(bytes) else {
        return StatusLine::Unreadable;
    };
    match settings.get("statusLine") {
        None | Some(Value::Null) => StatusLine::Absent,
        Some(line) => {
            let command = line
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let a_command = line.get("type").and_then(Value::as_str) == Some("command");
            if a_command && is_ours(&command) {
                StatusLine::Ours(command)
            } else {
                StatusLine::Other(command)
            }
        }
    }
}

fn refused(why: &str) -> Error {
    Error::InvalidData(format!("Claude Code's settings were left alone: {why}"))
}

fn written(settings: &Map<String, Value>) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(settings).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// The settings with Piaz Switch's line running `command`: added where there
/// is none, brought up to date where it is ours, refused where it is
/// someone else's or the settings can't be read.
pub fn with_status_line(bytes: Option<&[u8]>, command: &str) -> Result<Vec<u8>> {
    let mut settings = parse(bytes).map_err(|()| refused("they aren't valid JSON"))?;
    if let StatusLine::Other(_) = status_line(bytes) {
        return Err(refused("they already have a status line"));
    }
    let line = serde_json::json!({ "type": "command", "command": command });
    match settings.get_mut("statusLine") {
        Some(existing) => *existing = line,
        None => {
            settings.insert("statusLine".into(), line);
        }
    }
    Ok(written(&settings))
}

/// The settings without Piaz Switch's line; none when there was nothing of
/// ours to take out. Someone else's line is never touched.
pub fn without_status_line(bytes: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    match status_line(bytes) {
        StatusLine::Ours(_) => {}
        StatusLine::Unreadable => return Err(refused("they aren't valid JSON")),
        StatusLine::Absent | StatusLine::Other(_) => return Ok(None),
    }
    let mut settings = parse(bytes).map_err(|()| refused("they aren't valid JSON"))?;
    settings.remove("statusLine");
    Ok(Some(written(&settings)))
}

/// What Claude Code's settings on this computer hold for the line.
pub fn read(host: &dyn Host) -> StatusLine {
    match host.read(&path(host)) {
        Ok(bytes) => status_line(bytes.as_deref()),
        Err(_) => StatusLine::Unreadable,
    }
}

pub fn add(host: &dyn Host, command: &str) -> Result<()> {
    let path = path(host);
    let bytes = host.read(&path)?;
    let new = with_status_line(bytes.as_deref(), command)?;
    host.write_atomic(&path, &new)
}

pub fn remove(host: &dyn Host) -> Result<()> {
    let path = path(host);
    let bytes = host.read(&path)?;
    match without_status_line(bytes.as_deref())? {
        Some(new) => host.write_atomic(&path, &new),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    const OURS: &str = "\"C:/Users/ben/AppData/Local/Piaz Switch/pitstop-app.exe\" statusline";

    fn keys(bytes: &[u8]) -> Vec<String> {
        match serde_json::from_slice::<Value>(bytes).unwrap() {
            Value::Object(map) => map.keys().cloned().collect(),
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn a_line_is_added_where_there_is_none_keeping_every_other_setting_in_order() {
        let before = br#"{"model":"opus","permissions":{"allow":["Bash"]},"env":{"A":"1"}}"#;
        let after = with_status_line(Some(before), OURS).unwrap();
        assert_eq!(keys(&after), ["model", "permissions", "env", "statusLine"]);
        assert_eq!(status_line(Some(&after)), StatusLine::Ours(OURS.into()));
        let value: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(value["permissions"]["allow"][0], "Bash");
        assert_eq!(value["statusLine"]["type"], "command");
    }

    #[test]
    fn missing_or_empty_settings_get_just_the_line() {
        for before in [None, Some(&b""[..]), Some(&b"  \n"[..])] {
            let after = with_status_line(before, OURS).unwrap();
            assert_eq!(keys(&after), ["statusLine"]);
        }
        assert_eq!(status_line(None), StatusLine::Absent);
    }

    #[test]
    fn someone_elses_line_or_unreadable_settings_are_never_changed() {
        let theirs = br#"{"statusLine":{"type":"command","command":"~/.claude/line.sh"}}"#;
        assert_eq!(
            status_line(Some(theirs)),
            StatusLine::Other("~/.claude/line.sh".into())
        );
        assert!(with_status_line(Some(theirs), OURS).is_err());
        assert_eq!(without_status_line(Some(theirs)).unwrap(), None);

        let broken = br#"{"model": "opus",,}"#;
        assert_eq!(status_line(Some(broken)), StatusLine::Unreadable);
        assert!(with_status_line(Some(broken), OURS).is_err());
        assert!(without_status_line(Some(broken)).is_err());
        assert_eq!(status_line(Some(b"[1,2]")), StatusLine::Unreadable);
    }

    #[test]
    fn our_line_is_brought_up_to_date_and_removed_alone() {
        let old = br#"{"model":"opus","statusLine":{"type":"command","command":"/old/piaz-switch statusline"},"env":{}}"#;
        let updated = with_status_line(Some(old), OURS).unwrap();
        assert_eq!(keys(&updated), ["model", "statusLine", "env"]);
        assert_eq!(status_line(Some(&updated)), StatusLine::Ours(OURS.into()));
        let removed = without_status_line(Some(&updated)).unwrap().unwrap();
        assert_eq!(keys(&removed), ["model", "env"]);
        assert_eq!(without_status_line(Some(&removed)).unwrap(), None);
    }

    #[test]
    fn the_command_quotes_the_program_for_the_shell() {
        let program = PathBuf::from("/Applications/Piaz Switch.app/Contents/MacOS/pitstop-app");
        let command = command_for(&program);
        assert!(command.starts_with('"'));
        assert!(command.ends_with("pitstop-app\" statusline"));
        assert!(is_ours(&command));
        // A quote or a dollar in the path can't end the quoting early.
        let odd = command_for(Path::new("/tmp/a\"b$c/piaz-switch"));
        assert!(odd.contains("a\\\"b\\$c"));
        assert!(!is_ours("bash ~/.claude/statusline.sh"));
        assert!(!is_ours("piaz-switch list"));
    }

    #[test]
    fn adding_and_removing_go_through_the_settings_file() {
        let host = FakeHost::new();
        host.put_file(path(&host), br#"{"theme":"dark"}"#.to_vec());
        add(&host, OURS).unwrap();
        assert_eq!(read(&host), StatusLine::Ours(OURS.into()));
        remove(&host).unwrap();
        assert_eq!(read(&host), StatusLine::Absent);
        assert_eq!(keys(&host.file(&path(&host)).unwrap()), ["theme"]);
    }
}
