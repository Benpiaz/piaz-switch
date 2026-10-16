//! In-memory [`Host`] for tests. No real IO, no real keystore.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::host::Host;

#[derive(Default)]
pub struct FakeHost {
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    keychain: RefCell<BTreeMap<String, String>>,
    keystore: RefCell<BTreeMap<String, String>>,
    fail_writes: RefCell<BTreeMap<PathBuf, u32>>,
    /// Paths that differ only in case are one file, as on Windows and macOS.
    fold_case: bool,
}

impl FakeHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// A file system that ignores case, like NTFS and APFS by default.
    pub fn case_insensitive() -> Self {
        Self {
            fold_case: true,
            ..Self::default()
        }
    }

    fn key(&self, path: &Path) -> PathBuf {
        if self.fold_case {
            PathBuf::from(path.to_string_lossy().to_lowercase())
        } else {
            path.to_path_buf()
        }
    }

    pub fn put_file(&self, path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) {
        let key = self.key(&path.into());
        self.files.borrow_mut().insert(key, bytes.into());
    }

    pub fn file(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.borrow().get(&self.key(path)).cloned()
    }

    /// Make the next `times` writes to `path` fail with an injected IO error.
    pub fn fail_next_writes(&self, path: impl Into<PathBuf>, times: u32) {
        self.fail_writes.borrow_mut().insert(path.into(), times);
    }

    pub fn delete_keystore_entry(&self, name: &str) {
        self.keystore.borrow_mut().remove(name);
    }

    pub fn keychain_value(&self, service: &str) -> Option<String> {
        self.keychain.borrow().get(service).cloned()
    }
}

impl Host for FakeHost {
    fn home(&self) -> PathBuf {
        PathBuf::from("/fake-home")
    }

    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        Ok(self.files.borrow().get(&self.key(path)).cloned())
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let mut fails = self.fail_writes.borrow_mut();
        if let Some(n) = fails.get_mut(path) {
            if *n > 0 {
                *n -= 1;
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source: std::io::Error::other("injected write failure"),
                });
            }
        }
        drop(fails);
        self.files
            .borrow_mut()
            .insert(self.key(path), bytes.to_vec());
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        self.files.borrow_mut().remove(&self.key(path));
        Ok(())
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        Ok(self
            .files
            .borrow()
            .keys()
            .filter(|p| p.parent() == Some(self.key(path).as_path()))
            .cloned()
            .collect())
    }

    fn keychain_get(&self, service: &str) -> Result<Option<String>> {
        Ok(self.keychain.borrow().get(service).cloned())
    }

    fn keychain_set(&self, service: &str, value: &str) -> Result<()> {
        self.keychain
            .borrow_mut()
            .insert(service.into(), value.into());
        Ok(())
    }

    fn keychain_delete(&self, service: &str) -> Result<()> {
        self.keychain.borrow_mut().remove(service);
        Ok(())
    }

    fn keystore_get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.keystore.borrow().get(name).cloned())
    }

    fn keystore_set(&self, name: &str, value: &str) -> Result<()> {
        self.keystore.borrow_mut().insert(name.into(), value.into());
        Ok(())
    }

    /// The file layout on every OS, so tests read the same everywhere.
    fn claude_token_in_keychain(&self) -> bool {
        false
    }
}
