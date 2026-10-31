use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

/// Everything pitstop-core does to the machine goes through this trait, so the
/// whole park/switch flow is testable in memory via `testing::FakeHost`.
pub trait Host {
    fn home(&self) -> PathBuf;
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>>;
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    /// Foreign keychain entries (Claude Code's own item on macOS).
    fn keychain_get(&self, service: &str) -> Result<Option<String>>;
    fn keychain_set(&self, service: &str, value: &str) -> Result<()>;
    /// Delete a foreign keychain item; one that is not there is not an error.
    fn keychain_delete(&self, service: &str) -> Result<()>;
    /// Pitstop's own secure storage (Credential Manager / Keychain via `keyring`).
    fn keystore_get(&self, name: &str) -> Result<Option<String>>;
    fn keystore_set(&self, name: &str, value: &str) -> Result<()>;
    /// Where editors keep their data: `%APPDATA%` on Windows,
    /// `~/Library/Application Support` on macOS, `~/.config` elsewhere.
    fn app_data(&self) -> PathBuf;
    /// The user data directory of `app`'s build in use: under
    /// [`Host::app_data`], in the folder that build keeps its data in (see
    /// [`crate::editor::App::data_folder`]).
    fn editor_data(&self, app: &crate::editor::App) -> PathBuf;
    /// `keys` from an editor's `state.vscdb` (see [`crate::vscdb`]); None
    /// when the database isn't there.
    fn vscdb_get(&self, db: &Path, keys: &[&str]) -> Result<Option<BTreeMap<String, String>>>;
    /// Set `set` and remove `remove` in an editor's `state.vscdb`, all or
    /// nothing, only while the editor is closed.
    fn vscdb_put(&self, db: &Path, set: &BTreeMap<String, String>, remove: &[&str]) -> Result<()>;
    /// Whether `provider`'s editor is open now, or might be: a check that
    /// can't tell says it is. Never true for a command-line tool.
    fn editor_running(&self, provider: crate::ProviderId) -> bool;
    /// Whether Claude Code keeps its token in the keychain rather than a
    /// file: on macOS it does.
    fn claude_token_in_keychain(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

/// Pitstop deletes only keychain items a private sign-in left behind; Claude
/// Code's own login item is never one of them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn deletable_keychain_item(service: &str) -> Result<()> {
    if service == crate::provider::claude_code::KEYCHAIN_SERVICE {
        return Err(Error::Keystore(format!("refusing to delete {service}")));
    }
    Ok(())
}

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Write `bytes` to a new file at `tmp` (owner-only on Unix from the start) and
/// flush it to disk, so a power cut never leaves an empty file renamed in.
fn write_synced(tmp: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(tmp).map_err(|e| io_err(tmp, e))?;
    file.write_all(bytes).map_err(|e| io_err(tmp, e))?;
    file.sync_all().map_err(|e| io_err(tmp, e))
}

/// Rename `tmp` over `path`. `fs::rename` replaces an existing file on every
/// OS, so `path` never goes missing in between. On Windows a scanner or the
/// tool reading the file can hold it for a moment: try again briefly.
fn replace(tmp: &Path, path: &Path) -> Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(tmp, path) {
            Ok(()) => return Ok(()),
            // Access denied, or a sharing or lock violation (32, 33).
            Err(e)
                if cfg!(windows)
                    && tries < 5
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || matches!(e.raw_os_error(), Some(32 | 33))) =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(40 * tries));
            }
            Err(e) => return Err(io_err(path, e)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RealHost {
    home_override: Option<PathBuf>,
}

impl RealHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point all relative-to-home paths at a sandbox directory (used by the CLI `--home` flag).
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            home_override: Some(home),
        }
    }
}

impl Host for RealHost {
    fn home(&self) -> PathBuf {
        self.home_override
            .clone()
            .or_else(dirs::home_dir)
            .expect("cannot determine home directory")
    }

    fn app_data(&self) -> PathBuf {
        match &self.home_override {
            // A sandboxed home keeps the editors' data inside it too.
            Some(home) if cfg!(windows) => home.join("AppData").join("Roaming"),
            Some(home) if cfg!(target_os = "macos") => {
                home.join("Library").join("Application Support")
            }
            Some(home) => home.join(".config"),
            None => dirs::config_dir().unwrap_or_else(|| self.home().join(".config")),
        }
    }

    fn editor_data(&self, app: &crate::editor::App) -> PathBuf {
        // Programs live under the real home, whatever a sandboxed one says.
        let home = dirs::home_dir().unwrap_or_default();
        self.app_data().join(app.data_folder(&home))
    }

    fn vscdb_get(&self, db: &Path, keys: &[&str]) -> Result<Option<BTreeMap<String, String>>> {
        crate::vscdb::get(db, keys)
    }

    fn vscdb_put(&self, db: &Path, set: &BTreeMap<String, String>, remove: &[&str]) -> Result<()> {
        crate::vscdb::put(db, set, remove)
    }

    fn editor_running(&self, provider: crate::ProviderId) -> bool {
        crate::editor::app(provider).is_some_and(crate::editor::App::running)
    }

    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(path, e)),
        }
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidData(format!("path has no parent: {}", path.display())))?;
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        // One name per write, so two threads writing one path never share it.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "pitstop-tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written = write_synced(&tmp, bytes).and_then(|()| replace(&tmp, path));
        if written.is_err() {
            // The temporary file may hold a credential: never leave it behind.
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(path, e)),
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        match fs::read_dir(path) {
            Ok(entries) => entries
                .map(|e| e.map(|e| e.path()).map_err(|e| io_err(path, e)))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io_err(path, e)),
        }
    }

    #[cfg(target_os = "macos")]
    fn keychain_get(&self, service: &str) -> Result<Option<String>> {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .map_err(|e| Error::Keystore(format!("security CLI failed: {e}")))?;
        if out.status.success() {
            let mut value = String::from_utf8(out.stdout)
                .map_err(|_| Error::Keystore("keychain item is not UTF-8".into()))?;
            while value.ends_with('\n') || value.ends_with('\r') {
                value.pop();
            }
            Ok(Some(value))
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("could not be found") {
                Ok(None)
            } else {
                Err(Error::Keystore(format!(
                    "security find-generic-password: {err}"
                )))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn keychain_get(&self, _service: &str) -> Result<Option<String>> {
        Err(Error::Keystore(
            "OS keychain items are only used on macOS".into(),
        ))
    }

    #[cfg(target_os = "macos")]
    fn keychain_set(&self, service: &str, value: &str) -> Result<()> {
        let account = std::env::var("USER").unwrap_or_else(|_| "pitstop".into());
        let out = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-U",
                "-a",
                &account,
                "-s",
                service,
                "-w",
                value,
            ])
            .output()
            .map_err(|e| Error::Keystore(format!("security CLI failed: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Error::Keystore(format!(
                "security add-generic-password: {}",
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn keychain_set(&self, _service: &str, _value: &str) -> Result<()> {
        Err(Error::Keystore(
            "OS keychain items are only used on macOS".into(),
        ))
    }

    #[cfg(target_os = "macos")]
    fn keychain_delete(&self, service: &str) -> Result<()> {
        deletable_keychain_item(service)?;
        let out = std::process::Command::new("security")
            .args(["delete-generic-password", "-s", service])
            .output()
            .map_err(|e| Error::Keystore(format!("security CLI failed: {e}")))?;
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() || err.contains("could not be found") {
            Ok(())
        } else {
            Err(Error::Keystore(format!(
                "security delete-generic-password: {err}"
            )))
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn keychain_delete(&self, _service: &str) -> Result<()> {
        Err(Error::Keystore(
            "OS keychain items are only used on macOS".into(),
        ))
    }

    fn keystore_get(&self, name: &str) -> Result<Option<String>> {
        if let Some(value) = keystore_cache().get(name) {
            return Ok(Some(value.clone()));
        }
        let entry =
            keyring::Entry::new("pitstop", name).map_err(|e| Error::Keystore(e.to_string()))?;
        match entry.get_password() {
            Ok(v) => {
                keystore_cache().insert(name.to_owned(), v.clone());
                Ok(Some(v))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Keystore(e.to_string())),
        }
    }

    fn keystore_set(&self, name: &str, value: &str) -> Result<()> {
        let entry =
            keyring::Entry::new("pitstop", name).map_err(|e| Error::Keystore(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| Error::Keystore(e.to_string()))?;
        // Not cached as written: the next read asks the keystore, so a value
        // another process wrote at the same moment wins in both.
        keystore_cache().remove(name);
        Ok(())
    }
}

/// What this process read from or wrote to the keystore. The vault key is read
/// for every parked account on every usage read; on macOS each read may ask
/// the user to allow it, so it is read once per run.
fn keystore_cache() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, String>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parents_and_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let host = RealHost::with_home(dir.path().to_path_buf());
        let path = dir.path().join("a/b/file.json");
        host.write_atomic(&path, b"one").unwrap();
        host.write_atomic(&path, b"two").unwrap();
        assert_eq!(host.read(&path).unwrap().unwrap(), b"two");
        // no temp file left behind
        let leftovers: Vec<_> = host
            .list_dir(path.parent().unwrap())
            .unwrap()
            .into_iter()
            .filter(|p| p != &path)
            .collect();
        assert!(leftovers.is_empty(), "leftover files: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let host = RealHost::with_home(dir.path().to_path_buf());
        let path = dir.path().join("auth.json");
        host.write_atomic(&path, b"secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn claude_codes_own_keychain_item_is_never_deleted() {
        assert!(matches!(
            deletable_keychain_item("Claude Code-credentials"),
            Err(Error::Keystore(_))
        ));
        assert!(deletable_keychain_item("Claude Code-credentials-a4bc5512").is_ok());
    }

    #[test]
    fn read_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let host = RealHost::with_home(dir.path().to_path_buf());
        assert!(host.read(&dir.path().join("nope.json")).unwrap().is_none());
    }
}
