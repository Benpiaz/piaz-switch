//! Claude Code provider.
//!
//! Active credential locations (verified on a real install, 2026-10):
//! - Windows/Linux: `~/.claude/.credentials.json` (JSON, key `claudeAiOauth`)
//! - macOS: Keychain generic password, service `Claude Code-credentials`
//! - All OSes: `~/.claude.json` key `oauthAccount` holds account metadata
//!   (emailAddress, …) that must switch together with the token.
//!
//! The vault blob is `{"credentials": <raw string>, "oauth_account": <value>}`.
//! Credentials are carried as an opaque string so schema changes upstream
//! cannot break a switch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::error::{Error, Result};
use crate::host::Host;

pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// The keychain item Claude Code stores a login in when `CLAUDE_CONFIG_DIR`
/// is `dir`: the first 8 hex digits of the SHA-256 of the path, hashed as
/// given. (Claude Code hashes its NFC form; the private directories Pitstop
/// makes are ASCII, which NFC leaves alone.)
pub fn signed_in_service(dir: &Path) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
    let suffix: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{KEYCHAIN_SERVICE}-{suffix}")
}

#[derive(Serialize, Deserialize)]
struct Blob {
    credentials: String,
    oauth_account: serde_json::Value,
}

pub struct ClaudeCode {
    /// Whether the token is in the Keychain instead of a file; `None` asks
    /// the host, which knows its OS.
    pub use_keychain: Option<bool>,
}

impl ClaudeCode {
    pub fn for_this_os() -> Self {
        Self { use_keychain: None }
    }

    fn keychain(&self, host: &dyn Host) -> bool {
        self.use_keychain
            .unwrap_or_else(|| host.claude_token_in_keychain())
    }

    pub fn credentials_path(host: &dyn Host) -> PathBuf {
        host.home().join(".claude").join(".credentials.json")
    }

    pub fn config_path(host: &dyn Host) -> PathBuf {
        host.home().join(".claude.json")
    }

    /// Credentials plus the `oauthAccount` of the config at `config_path`, as
    /// one vault blob.
    fn pack(
        &self,
        host: &dyn Host,
        credentials: Option<String>,
        config_path: &Path,
    ) -> Result<Option<ActiveCredential>> {
        let Some(credentials) = credentials else {
            return Ok(None);
        };

        // A config that can't be read is an error, not an account without
        // identity: parked as one, switching back would drop `oauthAccount`.
        let oauth_account = read_config(host, config_path)?
            .and_then(|cfg| cfg.get("oauthAccount").cloned())
            .unwrap_or(serde_json::Value::Null);

        let blob = serde_json::to_vec(&Blob {
            credentials,
            oauth_account,
        })
        .map_err(|e| Error::InvalidData(format!("claude-code blob: {e}")))?;
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    /// The token a run with `CLAUDE_CONFIG_DIR=dir` keeps, if any.
    fn token_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<String>> {
        if self.keychain(host) {
            host.keychain_get(&signed_in_service(dir))
        } else {
            Ok(host
                .read(&dir.join(".credentials.json"))?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
        }
    }

    /// Hand the login in `blob` to a Claude Code run with `CLAUDE_CONFIG_DIR=dir`:
    /// its token where that run looks for one, its account in `dir/.claude.json`.
    /// [`Provider::discard_signed_in`] takes the token away again.
    pub fn lend(&self, host: &dyn Host, dir: &Path, blob: &[u8]) -> Result<()> {
        let blob: Blob = serde_json::from_slice(blob)
            .map_err(|e| Error::InvalidData(format!("claude-code blob: {e}")))?;
        if self.keychain(host) {
            host.keychain_set(&signed_in_service(dir), &blob.credentials)?;
        } else {
            host.write_atomic(&dir.join(".credentials.json"), blob.credentials.as_bytes())?;
        }
        let mut config = serde_json::Map::new();
        if !blob.oauth_account.is_null() {
            config.insert("oauthAccount".into(), blob.oauth_account);
        }
        let bytes = serde_json::to_vec(&config)
            .map_err(|e| Error::InvalidData(format!("claude-code config: {e}")))?;
        host.write_atomic(&dir.join(".claude.json"), &bytes)
    }

    /// `blob` with the token a run lent it left in `dir`, when Claude Code
    /// renewed it meanwhile; `None` when it is unchanged, gone or no token.
    pub fn renewed(&self, host: &dyn Host, dir: &Path, blob: &[u8]) -> Result<Option<Vec<u8>>> {
        let old: Blob = serde_json::from_slice(blob)
            .map_err(|e| Error::InvalidData(format!("claude-code blob: {e}")))?;
        let Some(now) = self.token_in(host, dir)? else {
            return Ok(None);
        };
        let has_token = serde_json::from_str::<serde_json::Value>(&now)
            .ok()
            .and_then(|v| {
                v.get("claudeAiOauth")?
                    .get("accessToken")?
                    .as_str()
                    .map(|_| ())
            })
            .is_some();
        if now == old.credentials || !has_token {
            return Ok(None);
        }
        let blob = serde_json::to_vec(&Blob {
            credentials: now,
            oauth_account: old.oauth_account,
        })
        .map_err(|e| Error::InvalidData(format!("claude-code blob: {e}")))?;
        Ok(Some(blob))
    }
}

impl Provider for ClaudeCode {
    fn id(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        let credentials = if self.keychain(host) {
            host.keychain_get(KEYCHAIN_SERVICE)?
        } else {
            host.read(&Self::credentials_path(host))?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        self.pack(host, credentials, &Self::config_path(host))
    }

    /// `claude auth login` with `CLAUDE_CONFIG_DIR=dir` keeps the token in
    /// `dir/.credentials.json` (macOS: the keychain item named after `dir`)
    /// and the account in `dir/.claude.json`.
    fn read_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<ActiveCredential>> {
        let credentials = if self.keychain(host) {
            host.keychain_get(&signed_in_service(dir))?
        } else {
            host.read(&dir.join(".credentials.json"))?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        self.pack(host, credentials, &dir.join(".claude.json"))
    }

    /// The keychain item outlives the directory unless deleted by name. Never
    /// Claude Code's own item, nor the one this process's own
    /// `CLAUDE_CONFIG_DIR` would name.
    fn discard_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<()> {
        if !self.keychain(host) {
            return Ok(()); // the file goes with the directory
        }
        let service = signed_in_service(dir);
        let own = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|d| !d.is_empty())
            .map(|d| signed_in_service(Path::new(&d)));
        if service == KEYCHAIN_SERVICE || own.as_ref() == Some(&service) {
            return Err(Error::Keystore(format!("refusing to delete {service}")));
        }
        host.keychain_delete(&service)
    }

    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        let blob: Blob = serde_json::from_slice(blob)
            .map_err(|e| Error::InvalidData(format!("claude-code blob: {e}")))?;

        // Swap only the oauthAccount key; the rest of ~/.claude.json is the
        // user's shared config and must survive byte-for-byte in meaning.
        // Checked before anything is written, so a config that can't be read
        // leaves the token alone too.
        let config_path = Self::config_path(host);
        let as_object = |config: Option<serde_json::Value>| match config {
            None => Ok(serde_json::json!({})),
            Some(config) if config.is_object() => Ok(config),
            Some(_) => Err(Error::InvalidData(
                "~/.claude.json is not a JSON object".into(),
            )),
        };
        as_object(read_config(host, &config_path)?)?;

        if self.keychain(host) {
            host.keychain_set(KEYCHAIN_SERVICE, &blob.credentials)?;
        } else {
            host.write_atomic(&Self::credentials_path(host), blob.credentials.as_bytes())?;
        }

        // Read again: writing the token can wait on a Keychain prompt, and
        // what Claude Code saved meanwhile must not be put back as it was.
        let mut config = as_object(read_config(host, &config_path)?)?;
        let obj = config.as_object_mut().expect("checked to be an object");
        if blob.oauth_account.is_null() {
            obj.remove("oauthAccount");
        } else {
            obj.insert("oauthAccount".into(), blob.oauth_account);
        }
        // Indented as Claude Code writes it, so the file stays readable.
        let bytes = serde_json::to_vec_pretty(&config)
            .map_err(|e| Error::InvalidData(format!("~/.claude.json: {e}")))?;
        host.write_atomic(&config_path, &bytes)
    }

    /// Identity from `oauthAccount` (`accountUuid:organizationUuid`, the
    /// organization half empty until Claude Code has fetched it), plan from
    /// `claudeAiOauth`: `subscriptionType`, plus the `20x` of a
    /// `rateLimitTier` such as `default_claude_max_20x`.
    fn details(&self, blob: &[u8]) -> Details {
        let Ok(blob) = serde_json::from_slice::<Blob>(blob) else {
            return Details::default();
        };
        let account = &blob.oauth_account;
        let identity = super::text(account, "accountUuid").map(|uuid| {
            let org = super::text(account, "organizationUuid").unwrap_or_default();
            format!("{uuid}:{org}")
        });

        let oauth = serde_json::from_str::<serde_json::Value>(&blob.credentials)
            .ok()
            .and_then(|c| c.get("claudeAiOauth").cloned())
            .unwrap_or(serde_json::Value::Null);
        let plan = super::text(&oauth, "subscriptionType").map(|kind| {
            let named = super::capitalized(&kind);
            let multiple = oauth
                .get("rateLimitTier")
                .and_then(|tier| tier.as_str())
                .and_then(|tier| tier.rsplit('_').next())
                .filter(|last| {
                    last.len() > 1
                        && last.ends_with('x')
                        && last[..last.len() - 1].bytes().all(|b| b.is_ascii_digit())
                });
            match multiple {
                Some(multiple) => format!("{named} {multiple}"),
                None => named,
            }
        });

        Details {
            label: super::text(account, "emailAddress"),
            identity,
            plan,
        }
    }
}

/// `~/.claude.json`, or None when there is none. Claude Code rewrites it often:
/// a read that lands mid-write doesn't parse, so it is read again a few times
/// before it counts as broken. A byte-order mark is skipped.
fn read_config(host: &dyn Host, path: &Path) -> Result<Option<serde_json::Value>> {
    let mut tries = 0;
    loop {
        let Some(bytes) = host.read(path)? else {
            return Ok(None);
        };
        let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        match serde_json::from_slice(text) {
            Ok(config) => return Ok(Some(config)),
            Err(_) if tries < 3 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100 * tries));
            }
            Err(e) => return Err(Error::InvalidData(format!("~/.claude.json: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    const CREDS_A: &str = r#"{"claudeAiOauth":{"accessToken":"TOKEN-A","refreshToken":"R-A"}}"#;

    fn config_with_email(email: &str) -> String {
        format!(
            r#"{{"numStartups":42,"oauthAccount":{{"accountUuid":"u-1","emailAddress":"{email}"}},"projects":{{"deep":"untouched"}}}}"#
        )
    }

    fn file_provider() -> ClaudeCode {
        ClaudeCode {
            use_keychain: Some(false),
        }
    }

    fn keychain_provider() -> ClaudeCode {
        ClaudeCode {
            use_keychain: Some(true),
        }
    }

    #[test]
    fn read_active_returns_none_when_logged_out() {
        let host = FakeHost::new();
        assert!(file_provider().read_active(&host).unwrap().is_none());
        assert!(keychain_provider().read_active(&host).unwrap().is_none());
    }

    #[test]
    fn read_active_from_file_carries_credentials_and_email_label() {
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), CREDS_A);
        host.put_file(ClaudeCode::config_path(&host), config_with_email("a@x.com"));
        let active = file_provider().read_active(&host).unwrap().unwrap();
        assert_eq!(active.details.label.as_deref(), Some("a@x.com"));
        // blob roundtrips through write_active below; here just check it embeds the raw creds
        let blob: serde_json::Value = serde_json::from_slice(&active.blob).unwrap();
        assert_eq!(blob["credentials"], CREDS_A);
        assert_eq!(blob["oauth_account"]["emailAddress"], "a@x.com");
    }

    #[test]
    fn a_config_with_a_byte_order_mark_still_gives_the_identity() {
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), CREDS_A);
        let mut config = vec![0xEF, 0xBB, 0xBF];
        config.extend_from_slice(config_with_email("a@x.com").as_bytes());
        host.put_file(ClaudeCode::config_path(&host), config);
        let active = file_provider().read_active(&host).unwrap().unwrap();
        assert_eq!(active.details.label.as_deref(), Some("a@x.com"));
    }

    #[test]
    fn a_config_that_does_not_parse_is_an_error_not_a_login_without_identity() {
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), CREDS_A);
        host.put_file(ClaudeCode::config_path(&host), r#"{"oauthAccount":{"accou"#);
        assert!(file_provider().read_active(&host).is_err());
    }

    #[test]
    fn write_active_leaves_the_token_alone_when_the_config_does_not_parse() {
        let host = FakeHost::new();
        host.put_file(ClaudeCode::credentials_path(&host), CREDS_A);
        host.put_file(ClaudeCode::config_path(&host), "{ not json");
        let blob = serde_json::to_vec(&serde_json::json!({
            "credentials": r#"{"claudeAiOauth":{"accessToken":"TOKEN-B"}}"#,
            "oauth_account": {"accountUuid": "u-2"},
        }))
        .unwrap();
        assert!(file_provider().write_active(&host, &blob).is_err());
        assert_eq!(
            host.file(&ClaudeCode::credentials_path(&host)).unwrap(),
            CREDS_A.as_bytes()
        );
    }

    #[test]
    fn read_active_from_keychain_on_macos_layout() {
        let host = FakeHost::new();
        host.keychain_set(KEYCHAIN_SERVICE, CREDS_A).unwrap();
        host.put_file(ClaudeCode::config_path(&host), config_with_email("a@x.com"));
        let active = keychain_provider().read_active(&host).unwrap().unwrap();
        let blob: serde_json::Value = serde_json::from_slice(&active.blob).unwrap();
        assert_eq!(blob["credentials"], CREDS_A);
    }

    #[test]
    fn write_active_restores_credentials_and_swaps_only_oauth_account() {
        let host = FakeHost::new();
        // current state: account B logged in, config has B's identity + other data
        host.put_file(
            ClaudeCode::credentials_path(&host),
            r#"{"claudeAiOauth":{"accessToken":"TOKEN-B"}}"#,
        );
        host.put_file(ClaudeCode::config_path(&host), config_with_email("b@x.com"));

        // blob of account A captured earlier
        let blob = serde_json::to_vec(&serde_json::json!({
            "credentials": CREDS_A,
            "oauth_account": {"accountUuid":"u-1","emailAddress":"a@x.com"},
        }))
        .unwrap();
        file_provider().write_active(&host, &blob).unwrap();

        let creds = host.file(&ClaudeCode::credentials_path(&host)).unwrap();
        assert_eq!(String::from_utf8(creds).unwrap(), CREDS_A);

        let cfg: serde_json::Value =
            serde_json::from_slice(&host.file(&ClaudeCode::config_path(&host)).unwrap()).unwrap();
        assert_eq!(cfg["oauthAccount"]["emailAddress"], "a@x.com");
        // the rest of ~/.claude.json must survive untouched
        assert_eq!(cfg["numStartups"], 42);
        assert_eq!(cfg["projects"]["deep"], "untouched");
    }

    /// The naming scheme was checked on a real Mac (2026-09-21): directories
    /// given to Claude Code as `CLAUDE_CONFIG_DIR` produced keychain items
    /// named exactly like this.
    #[test]
    fn keychain_item_names_match_what_claude_code_creates() {
        for (dir, service) in [
            ("/Users/me/.claude", "Claude Code-credentials-0bd83d07"),
            (
                "/private/tmp/claude-501/-Users-me/scratchpad/s1",
                "Claude Code-credentials-b082bd34",
            ),
            (
                "/Users/me/Library/Application Support/Pitstop/signin-1",
                "Claude Code-credentials-82000598",
            ),
        ] {
            assert_eq!(signed_in_service(Path::new(dir)), service, "{dir}");
        }
    }

    const NEW: &str = r#"{"claudeAiOauth":{"accessToken":"TOKEN-NEW","refreshToken":"R-NEW"}}"#;

    #[test]
    fn a_private_sign_in_is_read_from_its_directory() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-1");
        host.put_file(dir.join(".credentials.json"), NEW);
        host.put_file(dir.join(".claude.json"), config_with_email("n@x.com"));
        let signed_in = file_provider().read_signed_in(&host, dir).unwrap().unwrap();
        assert_eq!(signed_in.details.label.as_deref(), Some("n@x.com"));
        // a blob like any other: putting it in use restores exactly that login
        file_provider()
            .write_active(&host, &signed_in.blob)
            .unwrap();
        assert_eq!(
            host.file(&ClaudeCode::credentials_path(&host)).unwrap(),
            NEW.as_bytes()
        );
    }

    #[test]
    fn a_private_sign_in_on_macos_is_read_from_its_own_keychain_item() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-1");
        host.keychain_set(&signed_in_service(dir), NEW).unwrap();
        host.keychain_set(KEYCHAIN_SERVICE, CREDS_A).unwrap();
        host.put_file(dir.join(".claude.json"), config_with_email("n@x.com"));
        let signed_in = keychain_provider()
            .read_signed_in(&host, dir)
            .unwrap()
            .unwrap();
        let blob: serde_json::Value = serde_json::from_slice(&signed_in.blob).unwrap();
        assert_eq!(blob["credentials"], NEW);
    }

    #[test]
    fn discarding_a_private_sign_in_deletes_only_its_keychain_item() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-1");
        host.keychain_set(&signed_in_service(dir), NEW).unwrap();
        host.keychain_set(KEYCHAIN_SERVICE, CREDS_A).unwrap();
        keychain_provider().discard_signed_in(&host, dir).unwrap();
        assert_eq!(host.keychain_value(&signed_in_service(dir)), None);
        assert_eq!(
            host.keychain_value(KEYCHAIN_SERVICE).as_deref(),
            Some(CREDS_A)
        );
    }

    #[test]
    fn a_lent_login_comes_back_with_the_token_claude_code_renewed() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/piaz-warm-1");
        host.put_file(ClaudeCode::config_path(&host), config_with_email("a@x.com"));
        host.put_file(ClaudeCode::credentials_path(&host), CREDS_A);
        let parked = file_provider().read_active(&host).unwrap().unwrap().blob;

        file_provider().lend(&host, dir, &parked).unwrap();
        assert_eq!(
            host.file(&dir.join(".credentials.json")).unwrap(),
            CREDS_A.as_bytes()
        );
        let lent = file_provider().read_signed_in(&host, dir).unwrap().unwrap();
        assert_eq!(lent.details.label.as_deref(), Some("a@x.com"));
        // nothing renewed: nothing to keep
        assert_eq!(file_provider().renewed(&host, dir, &parked).unwrap(), None);

        host.put_file(dir.join(".credentials.json"), NEW);
        let back = file_provider()
            .renewed(&host, dir, &parked)
            .unwrap()
            .unwrap();
        let back: serde_json::Value = serde_json::from_slice(&back).unwrap();
        assert_eq!(back["credentials"], NEW);
        assert_eq!(back["oauth_account"]["emailAddress"], "a@x.com");
        // Claude Code's own login is left as it was
        assert_eq!(
            host.file(&ClaudeCode::credentials_path(&host)).unwrap(),
            CREDS_A.as_bytes()
        );
    }

    #[test]
    fn a_lent_login_on_macos_goes_in_its_own_keychain_item_and_garbage_is_not_kept() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/piaz-warm-2");
        let parked = serde_json::to_vec(&serde_json::json!({
            "credentials": CREDS_A,
            "oauth_account": {"emailAddress": "a@x.com"},
        }))
        .unwrap();
        keychain_provider().lend(&host, dir, &parked).unwrap();
        assert_eq!(
            host.keychain_value(&signed_in_service(dir)).as_deref(),
            Some(CREDS_A)
        );
        assert_eq!(host.keychain_value(KEYCHAIN_SERVICE), None);

        host.keychain_set(&signed_in_service(dir), "{\"other\":1}")
            .unwrap();
        assert_eq!(
            keychain_provider().renewed(&host, dir, &parked).unwrap(),
            None
        );
        host.keychain_set(&signed_in_service(dir), NEW).unwrap();
        assert!(keychain_provider()
            .renewed(&host, dir, &parked)
            .unwrap()
            .is_some());
    }

    fn blob_with(oauth_account: serde_json::Value, credentials: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "credentials": credentials.to_string(),
            "oauth_account": oauth_account,
        }))
        .unwrap()
    }

    #[test]
    fn details_name_the_account_its_organization_and_plan() {
        let blob = blob_with(
            serde_json::json!({"accountUuid": "acc-1", "organizationUuid": "org-1", "emailAddress": "a@x.com"}),
            serde_json::json!({"claudeAiOauth": {"accessToken": "SECRET", "subscriptionType": "max", "rateLimitTier": "default_claude_max_20x"}}),
        );
        assert_eq!(
            file_provider().details(&blob),
            Details {
                label: Some("a@x.com".into()),
                identity: Some("acc-1:org-1".into()),
                plan: Some("Max 20x".into()),
            }
        );
    }

    #[test]
    fn one_account_in_two_organizations_has_two_identities() {
        let in_org = |org: &str| {
            blob_with(
                serde_json::json!({"accountUuid": "acc-1", "organizationUuid": org, "emailAddress": "a@x.com"}),
                serde_json::json!({}),
            )
        };
        assert_eq!(
            file_provider()
                .details(&in_org("org-1"))
                .identity
                .as_deref(),
            Some("acc-1:org-1")
        );
        assert_eq!(
            file_provider()
                .details(&in_org("org-2"))
                .identity
                .as_deref(),
            Some("acc-1:org-2")
        );
    }

    #[test]
    fn an_organization_not_written_yet_leaves_its_half_empty() {
        let blob = blob_with(
            serde_json::json!({"accountUuid": "acc-1", "emailAddress": "a@x.com"}),
            serde_json::json!({}),
        );
        assert_eq!(
            file_provider().details(&blob).identity.as_deref(),
            Some("acc-1:")
        );
    }

    #[test]
    fn a_plan_without_a_multiplier_is_the_subscription_alone() {
        let blob = blob_with(
            serde_json::Value::Null,
            serde_json::json!({"claudeAiOauth": {"subscriptionType": "pro", "rateLimitTier": "default_claude_ai"}}),
        );
        assert_eq!(file_provider().details(&blob).plan.as_deref(), Some("Pro"));
    }

    #[test]
    fn details_of_an_unreadable_blob_are_empty() {
        assert_eq!(file_provider().details(b"not json"), Details::default());
        let empty = blob_with(serde_json::Value::Null, serde_json::json!({}));
        assert_eq!(file_provider().details(&empty), Details::default());
    }

    #[test]
    fn write_active_uses_keychain_when_configured() {
        let host = FakeHost::new();
        host.put_file(ClaudeCode::config_path(&host), config_with_email("b@x.com"));
        let blob = serde_json::to_vec(&serde_json::json!({
            "credentials": CREDS_A,
            "oauth_account": {"emailAddress":"a@x.com"},
        }))
        .unwrap();
        keychain_provider().write_active(&host, &blob).unwrap();
        assert_eq!(
            host.keychain_value(KEYCHAIN_SERVICE).as_deref(),
            Some(CREDS_A)
        );
        assert!(host.file(&ClaudeCode::credentials_path(&host)).is_none());
    }
}
