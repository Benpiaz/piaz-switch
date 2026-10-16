//! Encrypted on-disk storage for parked credentials.
//!
//! Each blob is AES-256-GCM encrypted into `~/.pitstop/vault/<id>.bin`
//! (`nonce || ciphertext || tag`). The 32-byte key lives in the OS keystore via
//! `keyring` — credentials themselves are too big for Windows Credential
//! Manager blobs, the key is not.

use std::path::PathBuf;

use aes_gcm::aead::{Aead, Generate, KeyInit, Nonce};
use aes_gcm::{Aes256Gcm, Key};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use crate::error::{Error, Result};
use crate::host::Host;

const KEY_NAME: &str = "vault-key";
const NONCE_LEN: usize = 12;

pub fn vault_dir(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("vault")
}

fn blob_path(host: &dyn Host, id: &str) -> PathBuf {
    vault_dir(host).join(format!("{id}.bin"))
}

fn existing_key(host: &dyn Host) -> Result<Option<Aes256Gcm>> {
    match host.keystore_get(KEY_NAME)? {
        None => Ok(None),
        Some(b64) => {
            let bytes = B64
                .decode(b64.trim())
                .map_err(|_| Error::Keystore("vault key is not valid base64".into()))?;
            let cipher = Aes256Gcm::new_from_slice(&bytes)
                .map_err(|_| Error::Keystore("vault key has the wrong length".into()))?;
            Ok(Some(cipher))
        }
    }
}

fn require_key(host: &dyn Host) -> Result<Aes256Gcm> {
    existing_key(host)?.ok_or(Error::VaultKeyMissing)
}

/// Get the vault key, minting one only when no blobs exist yet — a missing
/// key with blobs on disk means those blobs are unrecoverable, which must
/// surface as an error, never as silent re-keying.
fn get_or_create_key(host: &dyn Host) -> Result<Aes256Gcm> {
    if let Some(cipher) = existing_key(host)? {
        return Ok(cipher);
    }
    if !host.list_dir(&vault_dir(host))?.is_empty() {
        return Err(Error::VaultKeyMissing);
    }
    let key = Key::<Aes256Gcm>::generate();
    host.keystore_set(KEY_NAME, &B64.encode(key))?;
    // Read back what the keystore kept: when two processes mint a key at once,
    // both go on with the one that won, never with a key that was overwritten.
    existing_key(host)?.ok_or(Error::VaultKeyMissing)
}

/// Encrypt and store a parked credential blob.
pub fn store(host: &dyn Host, id: &str, plaintext: &[u8]) -> Result<()> {
    let cipher = get_or_create_key(host)?;
    let nonce = Nonce::<Aes256Gcm>::generate();
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| Error::VaultCorrupt(id.into()))?;
    let mut file = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    file.extend_from_slice(&nonce);
    file.extend_from_slice(&ciphertext);
    host.write_atomic(&blob_path(host, id), &file)
}

/// Load and decrypt a parked credential blob.
pub fn load(host: &dyn Host, id: &str) -> Result<Vec<u8>> {
    let bytes = host
        .read(&blob_path(host, id))?
        .ok_or_else(|| Error::VaultBlobMissing(id.into()))?;
    let cipher = require_key(host)?;
    if bytes.len() < NONCE_LEN {
        return Err(Error::VaultCorrupt(id.into()));
    }
    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    let nonce = <&Nonce<Aes256Gcm>>::try_from(nonce).map_err(|_| Error::VaultCorrupt(id.into()))?;
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| Error::VaultCorrupt(id.into()))
}

pub fn delete(host: &dyn Host, id: &str) -> Result<()> {
    host.remove_file(&blob_path(host, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::testing::FakeHost;

    #[test]
    fn store_then_load_roundtrips_exact_bytes() {
        let host = FakeHost::new();
        let secret = br#"{"claudeAiOauth":{"accessToken":"sk-FAKE-123"}}"#;
        store(&host, "claude-code--work", secret).unwrap();
        assert_eq!(load(&host, "claude-code--work").unwrap(), secret);
    }

    /// The on-disk format must not change under a crypto crate bump: a blob
    /// written as `nonce || ciphertext || tag` (AES-256-GCM test case 15 from
    /// the GCM spec) still loads.
    #[test]
    fn loads_a_blob_in_the_standard_aes_gcm_format() {
        fn hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }
        let key = hex("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308");
        let plaintext = hex(concat!(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
            "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
        ));
        let file = hex(concat!(
            "cafebabefacedbaddecaf888",
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa",
            "8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad",
            "b094dac5d93471bdec1a502270e3cc6c",
        ));
        let host = FakeHost::new();
        host.keystore_set(KEY_NAME, &B64.encode(key)).unwrap();
        host.put_file(blob_path(&host, "codex--old"), file);
        assert_eq!(load(&host, "codex--old").unwrap(), plaintext);
    }

    #[test]
    fn a_key_of_the_wrong_length_is_a_keystore_error() {
        let host = FakeHost::new();
        host.keystore_set(KEY_NAME, &B64.encode([0u8; 16])).unwrap();
        assert!(matches!(existing_key(&host), Err(Error::Keystore(_))));
    }

    #[test]
    fn blob_on_disk_is_not_plaintext() {
        let host = FakeHost::new();
        store(&host, "codex--p", b"VERY-SECRET-TOKEN").unwrap();
        let on_disk = host.file(&blob_path(&host, "codex--p")).unwrap();
        let haystack = String::from_utf8_lossy(&on_disk).into_owned();
        assert!(!haystack.contains("VERY-SECRET-TOKEN"));
    }

    #[test]
    fn lost_key_with_existing_blobs_is_a_clear_error_and_blob_survives() {
        let host = FakeHost::new();
        store(&host, "codex--p", b"data").unwrap();
        host.delete_keystore_entry("vault-key");
        // load fails loudly
        assert!(matches!(
            load(&host, "codex--p"),
            Err(Error::VaultKeyMissing)
        ));
        // store must refuse to mint a fresh key while old blobs exist
        assert!(matches!(
            store(&host, "codex--q", b"x"),
            Err(Error::VaultKeyMissing)
        ));
        // the undecryptable blob was not overwritten or removed
        assert!(host.file(&blob_path(&host, "codex--p")).is_some());
    }

    #[test]
    fn missing_blob_is_reported_as_missing() {
        let host = FakeHost::new();
        store(&host, "codex--exists", b"x").unwrap(); // ensures a key exists
        assert!(matches!(
            load(&host, "codex--nope"),
            Err(Error::VaultBlobMissing(id)) if id == "codex--nope"
        ));
    }

    #[test]
    fn delete_removes_blob() {
        let host = FakeHost::new();
        store(&host, "codex--p", b"x").unwrap();
        delete(&host, "codex--p").unwrap();
        assert!(host.file(&blob_path(&host, "codex--p")).is_none());
    }
}
