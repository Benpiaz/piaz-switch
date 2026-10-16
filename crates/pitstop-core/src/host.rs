use std::fs;
use std::path::{Path, PathBuf};

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
        let tmp = path.with_extension(format!("pitstop-tmp-{}", std::process::id()));
        fs::write(&tmp, bytes).map_err(|e| io_err(&tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| io_err(&tmp, e))?;
        }
        // Windows cannot rename over an existing file; the brief gap is accepted
        // and every caller keeps an in-memory backup for rollback.
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path).map_err(|e| io_err(path, e))?;
        }
        fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
        Ok(())
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
        let entry =
            keyring::Entry::new("pitstop", name).map_err(|e| Error::Keystore(e.to_string()))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Keystore(e.to_string())),
        }
    }

    fn keystore_set(&self, name: &str, value: &str) -> Result<()> {
        let entry =
            keyring::Entry::new("pitstop", name).map_err(|e| Error::Keystore(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| Error::Keystore(e.to_string()))
    }
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
