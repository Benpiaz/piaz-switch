//! The editors among the tools (Cursor, Antigravity): desktop apps, not
//! command lines. Each keeps its login in its own `state.vscdb`, reads it once
//! when it starts and keeps it in memory from then on, so a login is only
//! changed while the editor is closed, and the editor opened again after.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::provider::ProviderId;

/// What Pitstop needs to know of one editor app.
pub struct App {
    pub provider: ProviderId,
    /// Its folder under the per-user app data (`%APPDATA%` and the like).
    pub data_folder: &'static str,
    /// Its program on Windows, under `%LOCALAPPDATA%\Programs` or Program Files.
    windows_exe: &'static str,
    /// Its bundle on macOS, `<name>.app`.
    mac_app: &'static str,
}

const CURSOR: App = App {
    provider: ProviderId::Cursor,
    data_folder: "Cursor",
    windows_exe: r"cursor\Cursor.exe",
    mac_app: "Cursor.app",
};

const ANTIGRAVITY: App = App {
    provider: ProviderId::Antigravity,
    data_folder: "Antigravity",
    windows_exe: r"Antigravity\Antigravity.exe",
    mac_app: "Antigravity.app",
};

/// The editor app `provider` is, or None for a command-line tool.
pub fn app(provider: ProviderId) -> Option<&'static App> {
    match provider {
        ProviderId::Cursor => Some(&CURSOR),
        ProviderId::Antigravity => Some(&ANTIGRAVITY),
        ProviderId::ClaudeCode | ProviderId::Codex => None,
    }
}

impl App {
    /// Its `state.vscdb` under a user data directory.
    pub fn state_db_in(user_data: &Path) -> PathBuf {
        user_data
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    }

    /// The `state.vscdb` it uses day to day.
    pub fn state_db(&self, host: &dyn Host) -> PathBuf {
        Self::state_db_in(&host.app_data().join(self.data_folder))
    }

    /// The name of its program, without the folder.
    fn file_name(&self) -> &'static str {
        if cfg!(windows) {
            self.windows_exe
                .rsplit('\\')
                .next()
                .unwrap_or(self.windows_exe)
        } else {
            self.mac_app.trim_end_matches(".app")
        }
    }

    /// Where it is installed, if anywhere: its program on Windows, its
    /// bundle on macOS.
    pub fn find(&self, home: &Path) -> Option<PathBuf> {
        let places: Vec<PathBuf> = if cfg!(windows) {
            let mut roots: Vec<PathBuf> = std::env::var_os("LOCALAPPDATA")
                .map(|d| PathBuf::from(d).join("Programs"))
                .into_iter()
                .collect();
            roots.push(home.join("AppData").join("Local").join("Programs"));
            for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                roots.extend(std::env::var_os(var).map(PathBuf::from));
            }
            roots.iter().map(|r| r.join(self.windows_exe)).collect()
        } else {
            vec![
                PathBuf::from("/Applications").join(self.mac_app),
                home.join("Applications").join(self.mac_app),
            ]
        };
        places.into_iter().find(|p| p.exists())
    }

    /// Whether it is running now. A check that can't be made says it is, so
    /// its login is never written under it.
    pub fn running(&self) -> bool {
        #[cfg(windows)]
        {
            let filter = format!("IMAGENAME eq {}", self.file_name());
            match quiet(Command::new("tasklist").args(["/FI", &filter, "/FO", "CSV", "/NH"]))
                .output()
            {
                Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                    .to_lowercase()
                    .contains(&self.file_name().to_lowercase()),
                _ => true,
            }
        }
        #[cfg(not(windows))]
        {
            // The main process runs from the bundle's MacOS folder; its
            // helpers run from Frameworks. pgrep: 0 found, 1 none, else failed.
            let pattern = format!("/{}/Contents/MacOS/", self.mac_app);
            match quiet(Command::new("pgrep").args(["-f", &pattern]))
                .stdout(Stdio::null())
                .status()
            {
                Ok(s) => s.code() != Some(1),
                Err(_) => true,
            }
        }
    }

    /// Ask it to close (its windows on Windows, Quit on macOS) and wait for
    /// it to be gone. Whether unsaved files are kept or asked about is its
    /// own setting; one that asks, or doesn't go within `wait`, is left open
    /// and this fails.
    pub fn quit(&self, wait: Duration) -> Result<()> {
        if !self.running() {
            return Ok(());
        }
        #[cfg(windows)]
        let _ = quiet(Command::new("taskkill").args(["/IM", self.file_name()])).status();
        #[cfg(not(windows))]
        let _ = quiet(
            Command::new("osascript").args(["-e", &format!("quit app \"{}\"", self.file_name())]),
        )
        .status();
        let started = Instant::now();
        while started.elapsed() < wait {
            std::thread::sleep(Duration::from_millis(400));
            if !self.running() {
                // Let it finish letting go of its files.
                std::thread::sleep(Duration::from_millis(600));
                return Ok(());
            }
        }
        Err(Error::EditorStillOpen(self.provider.display_name().into()))
    }

    /// Open it, on its own: it outlives Pitstop.
    pub fn open(&self, home: &Path) -> Result<()> {
        let found = self
            .find(home)
            .ok_or_else(|| Error::ToolNotFound(self.provider.display_name().into()))?;
        let mut cmd = if cfg!(windows) {
            Command::new(&found)
        } else {
            let mut open = Command::new("open");
            open.arg("-a").arg(&found);
            open
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(DETACHED_PROCESS);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| Error::SignIn(format!("could not open {}: {e}", found.display())))
    }

    /// A command that runs it with its own data in `user_data`, apart from
    /// the copy in daily use, for a private sign-in.
    /// Its home is the private directory too, so what it keeps under the
    /// home (Antigravity's `~/.gemini`) stays apart from the user's.
    pub fn private_command(&self, home: &Path, user_data: &Path) -> Result<Command> {
        let found = self
            .find(home)
            .ok_or_else(|| Error::ToolNotFound(self.provider.display_name().into()))?;
        let program = if cfg!(windows) {
            found
        } else {
            found.join("Contents").join("MacOS").join(self.file_name())
        };
        let mut cmd = Command::new(program);
        cmd.arg("--user-data-dir")
            .arg(user_data)
            .arg("--extensions-dir")
            .arg(user_data.join("extensions"))
            .arg("--new-window")
            .env("HOME", user_data.join("home"))
            .env("USERPROFILE", user_data.join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(cmd)
    }
}

/// A helper command that shows no console window.
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null()).stderr(Stdio::null())
}

/// Open `url` in the default browser.
pub fn open_in_browser(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(Error::SignIn("refusing to open a non-https address".into()));
    }
    #[cfg(windows)]
    let mut cmd = {
        // Not through cmd.exe, which would read the `&`s in the address.
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    quiet(&mut cmd)
        .stdout(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| Error::SignIn(format!("could not open the browser: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    #[test]
    fn only_the_editors_are_apps_and_each_has_its_database() {
        assert!(app(ProviderId::ClaudeCode).is_none());
        assert!(app(ProviderId::Codex).is_none());
        let host = FakeHost::new();
        let cursor = app(ProviderId::Cursor).unwrap();
        assert_eq!(
            cursor.state_db(&host),
            PathBuf::from("/fake-home/app-data/Cursor/User/globalStorage/state.vscdb")
        );
        assert_eq!(
            App::state_db_in(Path::new("/tmp/x")),
            PathBuf::from("/tmp/x/User/globalStorage/state.vscdb")
        );
    }

    #[test]
    fn only_https_addresses_are_opened() {
        assert!(open_in_browser("file:///etc/passwd").is_err());
        assert!(open_in_browser("javascript:alert(1)").is_err());
    }
}
