//! Adding an account through the tool's own sign-in, run in a private
//! directory so the login in use is never touched or revoked.
//!
//! As measured (Claude Code 2.1.289, Codex 0.160.0):
//! - `claude auth login` with `CLAUDE_CONFIG_DIR=<dir>` opens the browser,
//!   prints `If the browser didn't open, visit: <https address>` and
//!   `Paste code here if prompted > ` on stdout, and reads a pasted
//!   `code#state` from stdin; a refused code gets `Invalid code` on stderr and
//!   it keeps reading.
//! - `codex -c cli_auth_credentials_store=file login` with `CODEX_HOME=<dir>`
//!   opens the browser, prints the https address on stderr and reads nothing.
//!   It revokes whatever login its home holds before it signs in, which is
//!   why that home is always a fresh, empty directory and never the real one.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::provider::{provider_for, ProviderId};

pub fn program_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::ClaudeCode => "claude",
        ProviderId::Codex => "codex",
    }
}

/// Where installers put the tools, looked at after `PATH`: the native
/// installer's `~/.local/bin`, npm's global directory, Homebrew.
fn install_places(home: &Path) -> Vec<PathBuf> {
    let mut places = vec![home.join(".local").join("bin")];
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            places.push(PathBuf::from(appdata).join("npm"));
        }
    } else {
        places.push(PathBuf::from("/opt/homebrew/bin"));
        places.push(PathBuf::from("/usr/local/bin"));
        places.push(home.join(".npm-global").join("bin"));
        if let Ok(versions) = std::fs::read_dir(home.join(".nvm").join("versions").join("node")) {
            places.extend(versions.flatten().map(|v| v.path().join("bin")));
        }
    }
    places
}

/// The directories to look for a tool in: `PATH`, then on macOS the login
/// shell's `PATH` (an app opened from Finder starts with a bare one), then
/// where installers put the tools.
pub fn search_path(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    dirs.extend(login_shell_path());
    dirs.extend(install_places(home));
    let mut seen = HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    dirs
}

/// `$SHELL -l -c 'printf %s "$PATH"'`, bounded so a slow profile cannot hold
/// up a sign-in.
#[cfg(target_os = "macos")]
fn login_shell_path() -> Vec<PathBuf> {
    use std::time::{Duration, Instant};

    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let Ok(mut child) = Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    std::env::split_paths(out.trim()).collect()
}

/// The first `name` in `dirs`, with the extensions Windows runs.
pub fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|ext| format!("{name}.{ext}"))
            .collect()
    } else {
        vec![name.to_owned()]
    };
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|path| path.is_file())
}

/// Where `provider`'s tool is, if anywhere. `home` is the real home
/// directory, where installers put the tools.
pub fn find_program(provider: ProviderId, home: &Path) -> Option<PathBuf> {
    find_in(program_name(provider), &search_path(home))
}

/// The sign-in command for `provider`, with `dir` as the tool's home and
/// `search` as its `PATH` (an npm shim needs `node` on it).
pub fn command(provider: ProviderId, program: &Path, dir: &Path, search: &[PathBuf]) -> Command {
    let mut cmd = Command::new(program);
    match provider {
        ProviderId::ClaudeCode => {
            cmd.args(["auth", "login"])
                .env("CLAUDE_CONFIG_DIR", dir)
                .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR");
        }
        ProviderId::Codex => {
            // Unquoted, Codex reads the value as a plain string; quotes would
            // have to survive cmd.exe on the way to an npm shim.
            cmd.args(["-c", "cli_auth_credentials_store=file", "login"])
                .env("CODEX_HOME", dir)
                .current_dir(dir);
        }
    }
    if let Ok(path) = std::env::join_paths(search) {
        cmd.env("PATH", path);
    }
    cmd
}

/// What a running sign-in has said so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// The sign-in page, to open where no browser opened or to copy elsewhere.
    pub url: Option<String>,
    /// The tool reads a code pasted from the browser.
    pub wants_code: bool,
    /// The tool refused the code sent last.
    pub refused_code: bool,
}

/// Read what the tool printed: `said` is all of it, `since_code` what it
/// printed after the last code was sent.
pub fn progress(provider: ProviderId, said: &str, since_code: &str) -> Progress {
    let said = plain(said);
    let url = first_https(&said);
    match provider {
        ProviderId::ClaudeCode => Progress {
            url,
            wants_code: said.contains("Paste code"),
            refused_code: plain(since_code).contains("Invalid code"),
        },
        ProviderId::Codex => Progress {
            url,
            ..Progress::default()
        },
    }
}

/// Text without ANSI escapes: CSI sequences go, and OSC sequences (terminal
/// hyperlinks) go while the text they wrap stays.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn first_https(text: &str) -> Option<String> {
    let start = text.find("https://")?;
    Some(
        text[start..]
            .chars()
            .take_while(|c| !c.is_whitespace() && !c.is_control())
            .collect(),
    )
}

/// Read and clean up what a finished sign-in left in `dir`: the login as a
/// vault blob, for `Engine::add_signed_in` or `Engine::sign_in_again`.
pub fn take_login(host: &dyn Host, provider: ProviderId, dir: &Path) -> Result<Vec<u8>> {
    let prov = provider_for(provider);
    let read = prov.read_signed_in(host, dir);
    let _ = prov.discard_signed_in(host, dir);
    match read? {
        Some(login) => Ok(login.blob),
        None => Err(Error::SignIn(format!(
            "{} finished signing in, but Pitstop found no login it saved",
            provider.display_name()
        ))),
    }
}

/// A fresh, empty directory only this user can read, for `provider`'s
/// sign-in. Dropping it deletes the directory and whatever the sign-in left
/// outside it (Claude Code's keychain item on macOS), whether the sign-in
/// finished, failed or was cancelled.
pub struct PrivateDir {
    path: PathBuf,
    provider: ProviderId,
}

impl PrivateDir {
    pub fn create(provider: ProviderId) -> Result<PrivateDir> {
        Self::create_for(provider, "signin")
    }

    /// The same, named `pitstop-<purpose>-…` so what it is for shows.
    pub fn create_for(provider: ProviderId, purpose: &str) -> Result<PrivateDir> {
        use aes_gcm::aead::Generate;

        let bytes = <[u8; 8]>::generate();
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("pitstop-{purpose}-{suffix}"));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        // macOS keeps temp under the /var -> /private/var link. The tool gets
        // the resolved path, so the keychain item name Claude Code derives
        // from it is the one Pitstop derives, whether it resolves links or not.
        #[cfg(unix)]
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        Ok(PrivateDir { path, provider })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = provider_for(self.provider)
            .discard_signed_in(&crate::host::RealHost::new(), &self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A sign-in running in a private directory, its output piped back here.
/// Dropping it stops the tool and deletes the directory.
pub struct Session {
    provider: ProviderId,
    child: Child,
    stdin: Option<ChildStdin>,
    said: Arc<Mutex<String>>,
    code_sent_at: usize,
    // Last, so the directory goes after the tool has stopped.
    dir: PrivateDir,
}

impl Session {
    /// Start `provider`'s sign-in. `home` is the real home directory, where
    /// installers put the tools.
    pub fn start(provider: ProviderId, home: &Path) -> Result<Session> {
        let search = search_path(home);
        let program = find_in(program_name(provider), &search)
            .ok_or_else(|| Error::ToolNotFound(program_name(provider).into()))?;
        let dir = PrivateDir::create(provider)?;
        let mut cmd = command(provider, &program, dir.path(), &search);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Its own process group, so a cancel reaches what an npm shim starts.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::SignIn(format!("could not start {}: {e}", program.display())))?;

        let said = Arc::new(Mutex::new(String::new()));
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ];
        for mut pipe in pipes.into_iter().flatten() {
            let said = Arc::clone(&said);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = pipe.read(&mut buf) {
                    if let Ok(mut said) = said.lock() {
                        said.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            });
        }
        Ok(Session {
            provider,
            stdin: child.stdin.take(),
            child,
            said,
            code_sent_at: 0,
            dir,
        })
    }

    pub fn provider(&self) -> ProviderId {
        self.provider
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn said(&self) -> String {
        self.said.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn progress(&self) -> Progress {
        let said = self.said();
        let since = said.get(self.code_sent_at..).unwrap_or("");
        progress(self.provider, &said, since)
    }

    /// What the tool printed, escapes removed, for a failure message.
    pub fn output(&self) -> String {
        plain(&self.said())
    }

    /// Type the code from the browser into the tool.
    pub fn send_code(&mut self, code: &str) -> Result<()> {
        self.code_sent_at = self.said.lock().map(|s| s.len()).unwrap_or(0);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::SignIn("the sign-in no longer reads a code".into()))?;
        writeln!(stdin, "{}", code.trim())
            .and_then(|()| stdin.flush())
            .map_err(|e| Error::SignIn(format!("could not send the code: {e}")))
    }

    /// `Some(true)` once the tool finished signing in, `Some(false)` once it
    /// gave up, `None` while it runs.
    pub fn try_finish(&mut self) -> Result<Option<bool>> {
        self.child
            .try_wait()
            .map(|status| status.map(|s| s.success()))
            .map_err(|e| Error::SignIn(format!("lost track of the sign-in: {e}")))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            kill_tree(&mut self.child);
        }
        let _ = self.child.wait();
    }
}

/// Stop the tool and whatever it started: an npm shim is cmd.exe or node with
/// the real tool under it, and killing the shim alone leaves the tool running.
pub fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    let _ = child.kill();
}

/// Run the sign-in attached to this terminal, for the command line: the tool
/// prints its own address and reads its own code. Returns the directory
/// holding what it saved.
pub fn run_in_terminal(provider: ProviderId, home: &Path) -> Result<PrivateDir> {
    let search = search_path(home);
    let program = find_in(program_name(provider), &search)
        .ok_or_else(|| Error::ToolNotFound(program_name(provider).into()))?;
    let dir = PrivateDir::create(provider)?;
    let status = command(provider, &program, dir.path(), &search)
        .status()
        .map_err(|e| Error::SignIn(format!("could not start {}: {e}", program.display())))?;
    if !status.success() {
        return Err(Error::SignIn(format!(
            "{} did not finish signing in ({status})",
            program_name(provider)
        )));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    /// The manual address Claude Code 2.1.289 prints, values made up.
    const CLAUDE_ADDRESS: &str = "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Aprofile&state=s";

    fn claude_said(address: &str) -> String {
        format!(
            "Opening browser to sign in\u{2026}\nIf the browser didn't open, visit: {address}\nPaste code here if prompted > "
        )
    }

    #[test]
    fn claude_offers_its_address_and_a_code_field_together() {
        assert_eq!(
            progress(ProviderId::ClaudeCode, &claude_said(CLAUDE_ADDRESS), ""),
            Progress {
                url: Some(CLAUDE_ADDRESS.into()),
                wants_code: true,
                refused_code: false,
            }
        );
    }

    #[test]
    fn nothing_is_offered_before_the_tool_prints_it() {
        assert_eq!(
            progress(
                ProviderId::ClaudeCode,
                "Opening browser to sign in\u{2026}\n",
                ""
            ),
            Progress::default()
        );
    }

    #[test]
    fn an_address_printed_as_a_terminal_hyperlink_is_read_once() {
        let said = format!("visit: \x1b]8;;{CLAUDE_ADDRESS}\x07{CLAUDE_ADDRESS}\x1b]8;;\x07\n");
        assert_eq!(
            progress(ProviderId::ClaudeCode, &said, "").url.as_deref(),
            Some(CLAUDE_ADDRESS)
        );
    }

    #[test]
    fn a_refused_code_counts_only_after_the_last_code_sent() {
        let said = claude_said(CLAUDE_ADDRESS);
        assert!(!progress(ProviderId::ClaudeCode, &said, "").refused_code);
        let refused = "Invalid code. Please make sure the full code was copied.\n";
        assert!(progress(ProviderId::ClaudeCode, &said, refused).refused_code);
    }

    #[test]
    fn codex_offers_its_https_address_not_its_local_server() {
        let said = "Starting local login server on http://localhost:1455.\nIf your browser did not open, navigate to this URL to authenticate:\n\nhttps://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x&state=y\n";
        assert_eq!(
            progress(ProviderId::Codex, said, ""),
            Progress {
                url: Some(
                    "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x&state=y"
                        .into()
                ),
                wants_code: false,
                refused_code: false,
            }
        );
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn env(cmd: &Command, key: &str) -> Option<Option<PathBuf>> {
        cmd.get_envs()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.map(PathBuf::from))
    }

    #[test]
    fn claude_signs_in_with_its_config_in_the_private_directory() {
        let dir = Path::new("/tmp/pitstop-signin-1");
        let cmd = command(
            ProviderId::ClaudeCode,
            Path::new("/bin/claude"),
            dir,
            &[PathBuf::from("/bin")],
        );
        assert_eq!(args(&cmd), ["auth", "login"]);
        assert_eq!(env(&cmd, "CLAUDE_CONFIG_DIR"), Some(Some(dir.into())));
        // it would pin the login back to the real keychain item
        assert_eq!(env(&cmd, "CLAUDE_SECURESTORAGE_CONFIG_DIR"), Some(None));
    }

    #[test]
    fn codex_signs_in_with_its_home_and_a_file_store_in_the_private_directory() {
        let dir = Path::new("/tmp/pitstop-signin-2");
        let cmd = command(
            ProviderId::Codex,
            Path::new("/bin/codex"),
            dir,
            &[PathBuf::from("/bin")],
        );
        assert_eq!(
            args(&cmd),
            ["-c", "cli_auth_credentials_store=file", "login"]
        );
        assert_eq!(env(&cmd, "CODEX_HOME"), Some(Some(dir.into())));
        assert_eq!(cmd.get_current_dir(), Some(dir));
    }

    #[test]
    fn the_tool_runs_with_the_search_path_as_its_path() {
        let search = [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        let cmd = command(
            ProviderId::Codex,
            Path::new("/bin/codex"),
            Path::new("/tmp/x"),
            &search,
        );
        let path = env(&cmd, "PATH").flatten().unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            search.to_vec()
        );
    }

    #[test]
    fn the_first_directory_holding_the_program_wins() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let file = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        std::fs::write(a.path().join("other"), "").unwrap();
        std::fs::write(b.path().join(file), "").unwrap();
        let dirs = [a.path().to_path_buf(), b.path().to_path_buf()];
        assert_eq!(find_in("claude", &dirs), Some(b.path().join(file)));
        assert_eq!(find_in("codex", &dirs), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_finds_an_npm_cmd_shim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.cmd"), "").unwrap();
        assert_eq!(
            find_in("codex", &[dir.path().to_path_buf()]),
            Some(dir.path().join("codex.cmd"))
        );
    }

    /// Against the real `claude`, opt-in: `cargo test -p pitstop-core
    /// real_claude -- --ignored`. It may open a sign-in tab in the browser;
    /// nothing is signed in, and the login in use must not change.
    #[test]
    #[ignore = "runs the real claude; opens a browser tab"]
    fn real_claude_sign_in_prints_its_address_and_leaves_the_login_in_use_alone() {
        use crate::host::{Host, RealHost};
        use crate::provider::claude_code::ClaudeCode;

        let host = RealHost::new();
        let live_before = std::fs::read(ClaudeCode::credentials_path(&host)).ok();
        let home = dirs::home_dir().unwrap();
        let session = Session::start(ProviderId::ClaudeCode, &home).unwrap();
        let dir = session.dir().to_path_buf();
        assert!(dir.is_dir());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = session.progress();
        while seen.url.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
            seen = session.progress();
        }
        println!("claude said:\n{}", session.output());
        let url = seen.url.expect("claude printed no sign-in address");
        assert!(url.starts_with("https://"), "{url}");
        assert!(seen.wants_code);

        drop(session);
        assert!(!dir.exists(), "private directory left behind");
        assert_eq!(
            std::fs::read(ClaudeCode::credentials_path(&host)).ok(),
            live_before,
            "the login in use changed"
        );
        let _ = host.home();
    }

    #[test]
    fn take_login_hands_over_what_the_sign_in_saved() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-3");
        host.put_file(
            dir.join("auth.json"),
            r#"{"tokens":{"access_token":"SECRET"}}"#,
        );
        assert_eq!(
            take_login(&host, ProviderId::Codex, dir).unwrap(),
            br#"{"tokens":{"access_token":"SECRET"}}"#
        );
    }
}
