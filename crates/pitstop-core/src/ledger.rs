//! What the tools have used, read from the session logs they keep on this
//! computer: every request's tokens, when it ran, in which session, on
//! which model.
//! - Claude Code: `<config>/projects/**/*.jsonl`, one transcript per session
//!   (subagents in `<session>/subagents/`). A message is written as several
//!   lines, each repeating its usage as it stood then; the last one is final.
//! - Codex: `<home>/sessions/**/*.jsonl` and `archived_sessions/`, one
//!   rollout per session. Each `token_count` event carries the last
//!   request's usage; the model is the latest `turn_context`'s.
//!
//! Only numbers, model names and session ids are kept: never what was said.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::provider::ProviderId;
use crate::usage::parse_rfc3339;

/// A request's tokens, split the way they are priced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Input read fresh, neither from nor into the cache.
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    /// Output, reasoning included.
    pub output: u64,
}

impl Tokens {
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Everything the model read or wrote.
    pub fn processed(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read + self.output
    }

    /// The prompt as a whole: what decides a long-context price.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read
    }

    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_read += other.cache_read;
        self.output += other.output;
    }
}

/// One request.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Epoch seconds.
    pub at: i64,
    pub tool: ProviderId,
    pub session: Arc<str>,
    pub model: Arc<str>,
    pub tokens: Tokens,
}

/// Small strings repeated on every line (session, model) are shared.
#[derive(Debug, Default)]
struct Names(Vec<Arc<str>>);

impl Names {
    fn get(&mut self, name: &str) -> Arc<str> {
        if let Some(known) = self.0.iter().find(|n| &***n == name) {
            return known.clone();
        }
        let name: Arc<str> = Arc::from(name);
        self.0.push(name.clone());
        name
    }
}

/// A Claude Code transcript, read line by line.
#[derive(Debug, Default)]
struct ClaudeLog {
    entries: Vec<Entry>,
    /// `message.id` + `requestId` → its entry, so a later line of the same
    /// message replaces the usage of an earlier one.
    seen: HashMap<(String, String), usize>,
    names: Names,
}

#[derive(Deserialize)]
struct ClaudeLine {
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<u64>,
}

impl ClaudeLog {
    /// Take in one line of the transcript; `file_session` names the
    /// session when the line does not.
    fn line(&mut self, line: &str, file_session: &str) {
        if !line.contains("\"usage\"") {
            return;
        }
        let Ok(parsed) = serde_json::from_str::<ClaudeLine>(line) else {
            return;
        };
        let Some(message) = parsed.message else {
            return;
        };
        let (Some(usage), Some(at)) = (
            message.usage,
            parsed.timestamp.as_deref().and_then(parse_rfc3339),
        ) else {
            return;
        };
        let write = usage.cache_creation_input_tokens.unwrap_or(0);
        let write_1h = usage
            .cache_creation
            .and_then(|c| c.ephemeral_1h_input_tokens)
            .unwrap_or(0)
            .min(write);
        let tokens = Tokens {
            input: usage.input_tokens.unwrap_or(0),
            cache_write_5m: write - write_1h,
            cache_write_1h: write_1h,
            cache_read: usage.cache_read_input_tokens.unwrap_or(0),
            output: usage.output_tokens.unwrap_or(0),
        };
        let key = message
            .id
            .map(|id| (id, parsed.request_id.unwrap_or_default()));
        if let Some(&at_index) = key.as_ref().and_then(|k| self.seen.get(k)) {
            self.entries[at_index].tokens = tokens;
            return;
        }
        let session = self
            .names
            .get(parsed.session_id.as_deref().unwrap_or(file_session));
        let model = self
            .names
            .get(message.model.as_deref().unwrap_or("unknown"));
        if let Some(key) = key {
            self.seen.insert(key, self.entries.len());
        }
        self.entries.push(Entry {
            at,
            tool: ProviderId::ClaudeCode,
            session,
            model,
            tokens,
        });
    }
}

/// A Codex rollout, read line by line.
#[derive(Debug, Default)]
struct CodexLog {
    entries: Vec<Entry>,
    session: Option<Arc<str>>,
    model: Option<Arc<str>>,
    /// The running total of the last event counted: an event repeating it
    /// is the same request told twice.
    last_total: Option<CodexUsage>,
    names: Names,
}

#[derive(Deserialize)]
struct CodexLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    model: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
    last_token_usage: Option<CodexUsage>,
}

impl CodexLog {
    /// Take in one line of the rollout; `file_session` names the session
    /// until its `session_meta` does.
    fn line(&mut self, line: &str, file_session: &str) {
        if !(line.contains("token_count")
            || line.contains("turn_context")
            || line.contains("session_meta"))
        {
            return;
        }
        let Ok(CodexLine {
            timestamp,
            kind,
            payload: Some(payload),
        }) = serde_json::from_str::<CodexLine>(line)
        else {
            return;
        };
        match (kind.as_deref(), payload.kind.as_deref()) {
            (Some("session_meta"), _) => {
                if let Some(id) = payload.id.as_deref().filter(|id| !id.is_empty()) {
                    self.session = Some(self.names.get(id));
                }
            }
            (Some("turn_context"), _) => {
                if let Some(model) = payload.model.as_deref().filter(|m| !m.is_empty()) {
                    self.model = Some(self.names.get(model));
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let Some(CodexInfo {
                    total_token_usage: total,
                    last_token_usage: Some(last),
                }) = payload.info
                else {
                    return;
                };
                if total.is_some() && total == self.last_total {
                    return;
                }
                self.last_total = total;
                let Some(at) = timestamp.as_deref().and_then(parse_rfc3339) else {
                    return;
                };
                let session = match &self.session {
                    Some(session) => session.clone(),
                    None => self.names.get(file_session),
                };
                let model = match &self.model {
                    Some(model) => model.clone(),
                    None => self.names.get("unknown"),
                };
                // OpenAI counts cached and cache-written input inside
                // `input_tokens`, and reasoning inside `output_tokens`.
                let read = last.cached_input_tokens.min(last.input_tokens);
                let write = last.cache_write_input_tokens.min(last.input_tokens - read);
                self.entries.push(Entry {
                    at,
                    tool: ProviderId::Codex,
                    session,
                    model,
                    tokens: Tokens {
                        input: last.input_tokens - read - write,
                        cache_write_5m: write,
                        cache_write_1h: 0,
                        cache_read: read,
                        output: last.output_tokens,
                    },
                });
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct CodexUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

/// Where the logs are: Claude Code's `projects` folders and Codex's
/// `sessions` / `archived_sessions` folders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
}

impl Roots {
    /// Every folder searched, for an empty state to name.
    pub fn all(&self) -> impl Iterator<Item = &PathBuf> {
        self.claude.iter().chain(&self.codex)
    }
}

/// The folders the tools write their logs in under `home`, and wherever
/// `CLAUDE_CONFIG_DIR` (a comma-separated list) and `CODEX_HOME` point.
pub fn roots(home: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Roots {
    let mut claude: Vec<PathBuf> = env("CLAUDE_CONFIG_DIR")
        .map(|dirs| {
            dirs.to_string_lossy()
                .split(',')
                .map(str::trim)
                .filter(|dir| !dir.is_empty())
                .map(|dir| Path::new(dir).join("projects"))
                .collect()
        })
        .unwrap_or_default();
    for default in [
        home.join(".claude").join("projects"),
        home.join(".config").join("claude").join("projects"),
    ] {
        if !claude.contains(&default) {
            claude.push(default);
        }
    }
    let codex_home = env("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    Roots {
        claude,
        codex: vec![
            codex_home.join("sessions"),
            codex_home.join("archived_sessions"),
        ],
    }
}

/// How long a log may sit untouched and still be read: the longest range
/// the window shows.
pub const HORIZON: Duration = Duration::from_secs(91 * 24 * 3_600);

/// The logs read so far, and how far into each.
#[derive(Debug, Default)]
pub struct Ledger {
    files: HashMap<PathBuf, FileScan>,
}

#[derive(Debug)]
struct FileScan {
    /// Bytes taken in: up to the end of the last whole line.
    read: u64,
    log: Log,
}

#[derive(Debug)]
enum Log {
    Claude(ClaudeLog),
    Codex(CodexLog),
}

/// What a scan saw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    pub files: usize,
    /// Files or folders that could not be read.
    pub errors: Vec<String>,
}

impl Ledger {
    /// Read what is new in the logs under `roots`.
    pub fn scan(&mut self, roots: &Roots, now: SystemTime) -> Scan {
        let mut scan = Scan::default();
        let mut walked = HashSet::new();
        let mut found = Vec::new();
        for (dirs, tool) in [
            (&roots.claude, ProviderId::ClaudeCode),
            (&roots.codex, ProviderId::Codex),
        ] {
            for dir in dirs {
                // The same folder named two ways is read once.
                let Ok(dir) = fs::canonicalize(dir) else {
                    continue;
                };
                if walked.insert(dir.clone()) {
                    walk(&dir, tool, &mut found, &mut scan.errors);
                }
            }
        }

        let oldest = now.checked_sub(HORIZON).unwrap_or(SystemTime::UNIX_EPOCH);
        let current: Vec<(PathBuf, ProviderId, u64)> = found
            .into_iter()
            .filter(|f| f.modified >= oldest)
            .map(|f| (f.path, f.tool, f.len))
            .collect();
        let still_there: std::collections::HashSet<&PathBuf> =
            current.iter().map(|(p, _, _)| p).collect();
        self.files.retain(|path, _| still_there.contains(path));
        for (path, tool, len) in current {
            scan.files += 1;
            if let Err(e) = self.read(&path, tool, len) {
                scan.errors.push(format!("{}: {e}", path.display()));
            }
        }
        scan
    }

    /// Take in what `path` gained since it was last read, up to `len`.
    fn read(&mut self, path: &Path, tool: ProviderId, len: u64) -> std::io::Result<()> {
        let fresh = || FileScan {
            read: 0,
            log: match tool {
                ProviderId::ClaudeCode => Log::Claude(ClaudeLog::default()),
                ProviderId::Codex => Log::Codex(CodexLog::default()),
            },
        };
        let scan = self.files.entry(path.to_path_buf()).or_insert_with(fresh);
        if len < scan.read {
            // Rewritten from the start: what was read no longer stands.
            *scan = fresh();
        }
        if len == scan.read {
            return Ok(());
        }
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(scan.read))?;
        let mut bytes = Vec::new();
        file.take(len - scan.read).read_to_end(&mut bytes)?;
        // A line still being written waits for its end.
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            return Ok(());
        };
        let file_session = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for line in bytes[..end].split(|&b| b == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let line = line.trim_end_matches('\r');
            match &mut scan.log {
                Log::Claude(log) => log.line(line, &file_session),
                Log::Codex(log) => log.line(line, &file_session),
            }
        }
        scan.read += end as u64 + 1;
        Ok(())
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.files.values().flat_map(|f| match &f.log {
            Log::Claude(log) => log.entries.iter(),
            Log::Codex(log) => log.entries.iter(),
        })
    }
}

struct Found {
    path: PathBuf,
    tool: ProviderId,
    len: u64,
    modified: SystemTime,
}

/// Every `*.jsonl` under `dir`, however deep. Links are not followed.
fn walk(dir: &Path, tool: ProviderId, found: &mut Vec<Found>, errors: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(&path, tool, found, errors);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            found.push(Found {
                path,
                tool,
                len: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An assistant line as Claude Code 2.1 writes it, trimmed of fields
    /// Pitstop does not read but keeping the shape around them.
    fn claude_line(id: &str, request: &str, at: &str, usage: &str) -> String {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"userType":"external","cwd":"C:\\work","sessionId":"s-1","version":"2.1.300","gitBranch":"main","message":{{"model":"claude-opus-5-5","id":"{id}","type":"message","role":"assistant","content":[{{"type":"text","text":"SECRET-CONVERSATION"}}],"stop_reason":null,"usage":{usage}}},"requestId":"{request}","type":"assistant","uuid":"u","timestamp":"{at}"}}"#
        )
    }

    const USAGE: &str = r#"{"input_tokens":12,"cache_creation_input_tokens":300,"cache_read_input_tokens":4000,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":200},"output_tokens":50,"service_tier":"standard","server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},"speed":"standard"}"#;

    fn read_claude(lines: &[String]) -> Vec<Entry> {
        let mut log = ClaudeLog::default();
        for line in lines {
            log.line(line, "file-session");
        }
        log.entries
    }

    #[test]
    fn a_claude_line_becomes_an_entry_with_its_tokens_split_by_price() {
        let entries = read_claude(&[claude_line(
            "msg_1",
            "req_1",
            "2026-10-09T04:45:39.066Z",
            USAGE,
        )]);
        assert_eq!(
            entries,
            [Entry {
                at: 1_791_521_139,
                tool: ProviderId::ClaudeCode,
                session: "s-1".into(),
                model: "claude-opus-5-5".into(),
                tokens: Tokens {
                    input: 12,
                    cache_write_5m: 100,
                    cache_write_1h: 200,
                    cache_read: 4000,
                    output: 50,
                },
            }]
        );
    }

    #[test]
    fn cache_writes_without_their_split_are_five_minute_writes() {
        let usage = r#"{"input_tokens":1,"cache_creation_input_tokens":100,"cache_read_input_tokens":0,"output_tokens":2}"#;
        let entries = read_claude(&[claude_line("msg_1", "req_1", "2026-10-09T04:45:39Z", usage)]);
        assert_eq!(entries[0].tokens.cache_write_5m, 100);
        assert_eq!(entries[0].tokens.cache_write_1h, 0);
    }

    #[test]
    fn a_message_written_as_several_lines_counts_once_with_its_final_usage() {
        let first = r#"{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":900,"output_tokens":10}"#;
        let last = r#"{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":900,"output_tokens":250}"#;
        let entries = read_claude(&[
            claude_line("msg_1", "req_1", "2026-10-09T04:45:39Z", first),
            claude_line("msg_1", "req_1", "2026-10-09T04:46:10Z", last),
            claude_line("msg_2", "req_2", "2026-10-09T04:47:00Z", first),
        ]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tokens.output, 250);
        assert_eq!(entries[0].at, 1_791_521_139, "the message started then");
        assert_eq!(entries[1].tokens.output, 10);
    }

    #[test]
    fn lines_without_usage_or_unreadable_lines_are_skipped() {
        let user = r#"{"type":"user","message":{"role":"user","content":"what is usage?"},"timestamp":"2026-10-09T04:45:39Z","sessionId":"s-1"}"#;
        let entries = read_claude(&[
            user.to_owned(),
            "{not json".to_owned(),
            r#"{"type":"assistant","message":{"usage":{"input_tokens":1}}}"#.to_owned(),
            claude_line("msg_1", "req_1", "2026-10-09T04:45:39Z", USAGE),
        ]);
        assert_eq!(entries.len(), 1, "{entries:?}");
    }

    // ---------- Codex ----------

    fn session_meta(id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-10T14:20:17.562Z","type":"session_meta","payload":{{"id":"{id}","session_id":"{id}","timestamp":"2026-10-10T14:20:16.000Z","cwd":"C:\\work","originator":"codex_desktop","cli_version":"0.150.0","source":"vscode","model_provider":"openai","base_instructions":{{"text":"SECRET-INSTRUCTIONS"}},"history_mode":"full"}}}}"#
        )
    }

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-10T14:20:18.000Z","type":"turn_context","payload":{{"turn_id":"t","cwd":"C:\\work","current_date":"2026-10-10","timezone":"Asia/Bangkok","approval_policy":"on-request","sandbox_policy":{{"type":"workspace-write"}},"model":"{model}","effort":"high","summary":"auto"}}}}"#
        )
    }

    fn usage_json(input: u64, cached: u64, output: u64, reasoning: u64) -> String {
        format!(
            r#"{{"input_tokens":{input},"cached_input_tokens":{cached},"cache_write_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{}}}"#,
            input + output
        )
    }

    fn token_count(at: &str, total: &str, last: &str) -> String {
        format!(
            r#"{{"timestamp":"{at}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{total},"last_token_usage":{last},"model_context_window":272000}},"rate_limits":{{"primary":{{"used_percent":12.0,"window_minutes":300,"resets_at":1791540000}}}}}}}}"#
        )
    }

    fn read_codex(lines: &[String]) -> Vec<Entry> {
        let mut log = CodexLog::default();
        for line in lines {
            log.line(line, "rollout-file");
        }
        log.entries
    }

    #[test]
    fn a_codex_token_count_is_its_last_request_with_cached_input_taken_out() {
        let entries = read_codex(&[
            session_meta("019d-1"),
            turn_context("gpt-5.6-sol"),
            token_count(
                "2026-10-09T04:45:39Z",
                &usage_json(1000, 800, 50, 20),
                &usage_json(1000, 800, 50, 20),
            ),
        ]);
        assert_eq!(
            entries,
            [Entry {
                at: 1_791_521_139,
                tool: ProviderId::Codex,
                session: "019d-1".into(),
                model: "gpt-5.6-sol".into(),
                tokens: Tokens {
                    input: 200,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 800,
                    output: 50,
                },
            }]
        );
    }

    #[test]
    fn a_codex_event_repeating_the_running_total_is_not_counted_again() {
        let first = usage_json(1000, 800, 50, 0);
        let second_last = usage_json(500, 400, 10, 0);
        let second_total = usage_json(1500, 1200, 60, 0);
        let entries = read_codex(&[
            turn_context("gpt-5.6-sol"),
            token_count("2026-10-09T04:45:39Z", &first, &first),
            token_count("2026-10-09T04:45:40Z", &first, &first),
            token_count("2026-10-09T04:46:00Z", &second_total, &second_last),
        ]);
        let outputs: Vec<u64> = entries.iter().map(|e| e.tokens.output).collect();
        assert_eq!(outputs, [50, 10]);
    }

    #[test]
    fn codex_requests_run_on_the_model_of_the_latest_turn() {
        let one = usage_json(10, 0, 1, 0);
        let two = usage_json(20, 0, 2, 0);
        let entries = read_codex(&[
            turn_context("gpt-5.5"),
            token_count("2026-10-09T04:45:39Z", &one, &one),
            turn_context("gpt-6-luna"),
            token_count("2026-10-09T04:46:00Z", &two, &one),
        ]);
        let models: Vec<&str> = entries.iter().map(|e| &*e.model).collect();
        assert_eq!(models, ["gpt-5.5", "gpt-6-luna"]);
    }

    #[test]
    fn a_rollout_without_meta_or_turn_is_its_file_on_an_unknown_model() {
        let one = usage_json(10, 0, 1, 0);
        let empty_info = r#"{"timestamp":"2026-10-09T04:45:30Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}"#;
        let entries = read_codex(&[
            empty_info.to_owned(),
            token_count("2026-10-09T04:45:39Z", &one, &one),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(&*entries[0].session, "rollout-file");
        assert_eq!(&*entries[0].model, "unknown");
    }

    // ---------- scanning ----------

    use std::fs;
    use std::io::Write as _;

    fn append(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    struct Logs {
        _dir: tempfile::TempDir,
        roots: Roots,
        claude: PathBuf,
        codex: PathBuf,
    }

    fn logs() -> Logs {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude").join("projects");
        let codex = dir.path().join("codex").join("sessions");
        Logs {
            roots: Roots {
                claude: vec![claude.clone()],
                codex: vec![codex.clone()],
            },
            _dir: dir,
            claude,
            codex,
        }
    }

    fn usage_with_output(output: u64) -> String {
        format!(
            r#"{{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":{output}}}"#
        )
    }

    fn outputs(ledger: &Ledger) -> Vec<u64> {
        let mut outputs: Vec<u64> = ledger.entries().map(|e| e.tokens.output).collect();
        outputs.sort();
        outputs
    }

    #[test]
    fn a_scan_reads_both_tools_logs_in_every_subfolder() {
        let logs = logs();
        let at = "2026-10-09T04:45:39Z";
        append(
            &logs.claude.join("proj").join("s1.jsonl"),
            &format!("{}\n", claude_line("m1", "r1", at, &usage_with_output(1))),
        );
        append(
            &logs
                .claude
                .join("proj")
                .join("s1")
                .join("subagents")
                .join("agent-a.jsonl"),
            &format!("{}\n", claude_line("m2", "r2", at, &usage_with_output(2))),
        );
        let codex_usage = usage_json(10, 0, 3, 0);
        append(
            &logs
                .codex
                .join("2026")
                .join("10")
                .join("09")
                .join("rollout-x.jsonl"),
            &format!(
                "{}\n{}\n",
                turn_context("gpt-5.6-sol"),
                token_count(at, &codex_usage, &codex_usage)
            ),
        );
        append(&logs.claude.join("proj").join("notes.txt"), "not a log\n");

        let mut ledger = Ledger::default();
        let scan = ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(scan.files, 3);
        assert_eq!(outputs(&ledger), [1, 2, 3]);
    }

    #[test]
    fn a_later_scan_reads_only_what_was_written_since() {
        let logs = logs();
        let path = logs.claude.join("proj").join("s1.jsonl");
        let at = "2026-10-09T04:45:39Z";
        append(
            &path,
            &format!("{}\n", claude_line("m1", "r1", at, &usage_with_output(10))),
        );
        let mut ledger = Ledger::default();
        ledger.scan(&logs.roots, SystemTime::now());

        // The same message gets its final line, and a new one starts.
        append(
            &path,
            &format!("{}\n", claude_line("m1", "r1", at, &usage_with_output(250))),
        );
        append(
            &path,
            &format!("{}\n", claude_line("m2", "r2", at, &usage_with_output(7))),
        );
        ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(outputs(&ledger), [7, 250]);
    }

    #[test]
    fn a_growing_rollout_counts_each_request_once() {
        let logs = logs();
        let path = logs.codex.join("rollout-x.jsonl");
        let at = "2026-10-09T04:45:39Z";
        let (one, two, three) = (
            usage_json(10, 0, 1, 0),
            usage_json(20, 0, 2, 0),
            usage_json(30, 0, 3, 0),
        );
        append(
            &path,
            &format!(
                "{}\n{}\n{}\n",
                turn_context("gpt-5.6-sol"),
                token_count(at, &usage_json(10, 0, 1, 0), &one),
                token_count(at, &usage_json(30, 0, 3, 0), &two)
            ),
        );
        let mut ledger = Ledger::default();
        ledger.scan(&logs.roots, SystemTime::now());
        append(
            &path,
            &format!("{}\n", token_count(at, &usage_json(60, 0, 6, 0), &three)),
        );
        ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(outputs(&ledger), [1, 2, 3]);
    }

    #[test]
    fn a_line_still_being_written_waits_for_its_end() {
        let logs = logs();
        let path = logs.claude.join("proj").join("s1.jsonl");
        let line = claude_line("m1", "r1", "2026-10-09T04:45:39Z", &usage_with_output(5));
        let (head, tail) = line.split_at(line.len() / 2);
        append(&path, head);
        let mut ledger = Ledger::default();
        ledger.scan(&logs.roots, SystemTime::now());
        assert!(outputs(&ledger).is_empty());

        append(&path, &format!("{tail}\n"));
        ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(outputs(&ledger), [5]);
    }

    #[test]
    fn a_file_that_shrank_is_read_again_and_a_removed_one_is_forgotten() {
        let logs = logs();
        let at = "2026-10-09T04:45:39Z";
        let kept = logs.claude.join("proj").join("s1.jsonl");
        let removed = logs.claude.join("proj").join("s2.jsonl");
        append(
            &kept,
            &format!(
                "{}\n{}\n",
                claude_line("m1", "r1", at, &usage_with_output(1)),
                claude_line("m2", "r2", at, &usage_with_output(2))
            ),
        );
        append(
            &removed,
            &format!("{}\n", claude_line("m3", "r3", at, &usage_with_output(3))),
        );
        let mut ledger = Ledger::default();
        ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(outputs(&ledger), [1, 2, 3]);

        fs::write(
            &kept,
            format!("{}\n", claude_line("m9", "r9", at, &usage_with_output(9))),
        )
        .unwrap();
        fs::remove_file(&removed).unwrap();
        ledger.scan(&logs.roots, SystemTime::now());
        assert_eq!(outputs(&ledger), [9]);
    }

    #[test]
    fn logs_untouched_for_longer_than_the_horizon_are_not_read() {
        let logs = logs();
        let at = "2026-10-09T04:45:39Z";
        let old = logs.claude.join("proj").join("old.jsonl");
        append(
            &old,
            &format!("{}\n", claude_line("m1", "r1", at, &usage_with_output(1))),
        );
        append(
            &logs.claude.join("proj").join("new.jsonl"),
            &format!("{}\n", claude_line("m2", "r2", at, &usage_with_output(2))),
        );
        let now = SystemTime::now();
        fs::OpenOptions::new()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(now - HORIZON - Duration::from_secs(60))
            .unwrap();
        let mut ledger = Ledger::default();
        let scan = ledger.scan(&logs.roots, now);
        assert_eq!(outputs(&ledger), [2]);
        assert_eq!(scan.files, 1);
    }

    #[test]
    fn a_folder_named_twice_is_read_once() {
        let logs = logs();
        append(
            &logs.claude.join("proj").join("s1.jsonl"),
            &format!(
                "{}\n",
                claude_line("m1", "r1", "2026-10-09T04:45:39Z", &usage_with_output(1))
            ),
        );
        let twice = Roots {
            claude: vec![
                logs.claude.clone(),
                logs.claude.join(".").join("..").join("projects"),
            ],
            codex: vec![logs.codex.join("missing")],
        };
        let mut ledger = Ledger::default();
        let scan = ledger.scan(&twice, SystemTime::now());
        assert_eq!(outputs(&ledger), [1]);
        assert!(
            scan.errors.is_empty(),
            "a missing folder is not an error: {:?}",
            scan.errors
        );
    }

    #[test]
    fn logs_are_looked_for_where_the_tools_and_their_variables_put_them() {
        let home = Path::new("/h");
        let env = |name: &str| match name {
            "CLAUDE_CONFIG_DIR" => Some(OsString::from("/x, /y")),
            _ => None,
        };
        let found = roots(home, &env);
        assert_eq!(
            found.claude,
            [
                PathBuf::from("/x/projects"),
                PathBuf::from("/y/projects"),
                home.join(".claude").join("projects"),
                home.join(".config").join("claude").join("projects"),
            ]
        );
        assert_eq!(
            found.codex,
            [
                home.join(".codex").join("sessions"),
                home.join(".codex").join("archived_sessions"),
            ]
        );

        let codex_home = |name: &str| (name == "CODEX_HOME").then(|| OsString::from("/c"));
        assert_eq!(
            roots(home, &codex_home).codex,
            [
                PathBuf::from("/c/sessions"),
                PathBuf::from("/c/archived_sessions")
            ]
        );
    }

    #[test]
    fn a_line_without_a_session_belongs_to_its_file() {
        let line = r#"{"type":"assistant","message":{"id":"m","model":"claude-sonnet-5-5","usage":{"input_tokens":1,"output_tokens":1}},"timestamp":"2026-10-09T04:45:39Z"}"#;
        let entries = read_claude(&[line.to_owned()]);
        assert_eq!(&*entries[0].session, "file-session");
    }
}
