pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod cursor;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::host::Host;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderId {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "cursor")]
    Cursor,
    #[serde(rename = "antigravity")]
    Antigravity,
}

impl ProviderId {
    pub const ALL: [ProviderId; 4] = [
        ProviderId::ClaudeCode,
        ProviderId::Codex,
        ProviderId::Cursor,
        ProviderId::Antigravity,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "claude-code",
            ProviderId::Codex => "codex",
            ProviderId::Cursor => "cursor",
            ProviderId::Antigravity => "antigravity",
        }
    }

    /// An editor app rather than a command line (see [`crate::editor`]).
    pub fn is_editor(&self) -> bool {
        crate::editor::app(*self).is_some()
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "Claude Code",
            ProviderId::Codex => "Codex",
            ProviderId::Cursor => "Cursor",
            ProviderId::Antigravity => "Antigravity",
        }
    }

    pub fn parse(s: &str) -> Result<ProviderId> {
        match s {
            "claude-code" | "claude" => Ok(ProviderId::ClaudeCode),
            "codex" => Ok(ProviderId::Codex),
            "cursor" => Ok(ProviderId::Cursor),
            "antigravity" | "gemini" => Ok(ProviderId::Antigravity),
            other => Err(Error::InvalidData(format!(
                "unknown provider `{other}` (use claude-code, codex, cursor or antigravity)"
            ))),
        }
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The live credential of a tool, serialized to a provider-specific blob that
/// `write_active` can faithfully restore.
pub struct ActiveCredential {
    pub blob: Vec<u8>,
    pub details: Details,
}

/// Who a credential belongs to, read from the credential alone: no network,
/// and nothing secret, so all of it may go in `state.json` and to the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    /// The email address, for people to read.
    pub label: Option<String>,
    /// `<account>:<organization or workspace>`; either half may be empty when
    /// the credential does not say it.
    pub identity: Option<String>,
    /// The plan as people call it, e.g. `Max 20x` or `Pro`.
    pub plan: Option<String>,
}

pub trait Provider {
    fn id(&self) -> ProviderId;
    /// Read the tool's currently-active credential, or None when logged out.
    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>>;
    /// Put a previously-read blob back where the tool reads it.
    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()>;
    /// Who a blob belongs to. Unreadable parts are `None`, never an error.
    fn details(&self, blob: &[u8]) -> Details;
    /// The login a private sign-in (see `signin`) left for `dir`, packed as
    /// `read_active` packs the live one, or None when it saved nothing.
    fn read_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<ActiveCredential>>;
    /// Delete what a private sign-in in `dir` left outside that directory.
    fn discard_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<()>;
}

/// A non-empty string field of a JSON object, trimmed.
fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `max` → `Max`, the way plans are named on screen.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn provider_for(id: ProviderId) -> Box<dyn Provider> {
    match id {
        ProviderId::ClaudeCode => Box::new(claude_code::ClaudeCode::for_this_os()),
        ProviderId::Codex => Box::new(codex::Codex),
        ProviderId::Cursor => Box::new(cursor::Cursor),
        ProviderId::Antigravity => Box::new(antigravity::Antigravity),
    }
}
