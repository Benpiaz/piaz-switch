use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::provider::ProviderId;

/// Non-secret metadata only. Credentials never go in here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountMeta {
    pub name: String,
    pub provider: ProviderId,
    pub label: Option<String>,
    pub parked_at: Option<u64>,
    /// Who the parked login belongs to (see `provider::Details`). Absent in
    /// state written by 0.1.0 until the account is next parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub accounts: Vec<AccountMeta>,
    /// provider id -> name of the account currently holding the live credential.
    #[serde(default)]
    pub active: BTreeMap<String, String>,
}

pub fn state_path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("state.json")
}

impl State {
    pub fn load(host: &dyn Host) -> Result<State> {
        match host.read(&state_path(host))? {
            None => Ok(State::default()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::InvalidData(format!("state.json: {e}"))),
        }
    }

    pub fn save(&self, host: &dyn Host) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::InvalidData(format!("state.json: {e}")))?;
        host.write_atomic(&state_path(host), &bytes)
    }

    pub fn active_name(&self, provider: ProviderId) -> Option<&String> {
        self.active.get(provider.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    #[test]
    fn load_missing_state_is_empty() {
        let host = FakeHost::new();
        let state = State::load(&host).unwrap();
        assert!(state.accounts.is_empty());
        assert!(state.active.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let host = FakeHost::new();
        let mut state = State::default();
        state.accounts.push(AccountMeta {
            name: "work".into(),
            provider: ProviderId::ClaudeCode,
            label: Some("user@example.com".into()),
            parked_at: Some(1_760_000_000),
            identity: Some("acc-1:org-1".into()),
            plan: Some("Max 20x".into()),
        });
        state.active.insert("claude-code".into(), "work".into());
        state.save(&host).unwrap();
        assert_eq!(State::load(&host).unwrap(), state);
    }

    #[test]
    fn state_written_by_0_1_0_still_loads() {
        let host = FakeHost::new();
        host.put_file(
            state_path(&host),
            r#"{"accounts":[{"name":"work","provider":"claude-code","label":"a@x.com","parked_at":1760000000}],"active":{"claude-code":"work"}}"#,
        );
        let state = State::load(&host).unwrap();
        assert_eq!(state.accounts[0].identity, None);
        assert_eq!(state.accounts[0].plan, None);
        assert_eq!(state.active_name(ProviderId::ClaudeCode).unwrap(), "work");
    }
}
