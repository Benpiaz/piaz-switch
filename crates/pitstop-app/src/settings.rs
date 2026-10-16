//! The app's own preferences, in `~/.pitstop/app.json` beside the state.
//! Nothing secret goes here either.

use std::path::PathBuf;

use pitstop_core::error::Result;
use pitstop_core::Host;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// `en` or `vi`; none follows the system language.
    pub language: Option<String>,
    /// `light` or `dark`; none follows the system theme.
    pub theme: Option<String>,
    /// Tools whose "Add a second account" tip was dismissed.
    pub dismissed_tips: Vec<String>,
    /// Switch Claude Code by itself once a limit of the account in use
    /// reaches `auto_switch_at` percent.
    pub auto_switch: bool,
    pub auto_switch_at: u8,
    /// Notify when the account in use runs out and another has room.
    pub notify_run_out: bool,
    /// Claude Code accounts, by name, whose next five-hour period is started
    /// as soon as the last one ends.
    pub warm_up: Vec<String>,
    /// The span of the day warm-ups are made in, `HH:MM` local time; a span
    /// whose start is later than its end runs past midnight.
    pub warm_from: String,
    pub warm_to: String,
    /// Look for a newer Piaz Switch now and then, and say when there is one.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: None,
            theme: None,
            dismissed_tips: Vec::new(),
            auto_switch: false,
            auto_switch_at: 90,
            notify_run_out: true,
            warm_up: Vec::new(),
            warm_from: "07:00".into(),
            warm_to: "23:00".into(),
            check_updates: true,
        }
    }
}

impl Settings {
    /// Settings kept by account name follow it to its new name.
    pub fn rename_account(&mut self, old: &str, new: &str) {
        for name in &mut self.warm_up {
            if name == old {
                new.clone_into(name);
            }
        }
    }

    pub fn forget_account(&mut self, name: &str) {
        self.warm_up.retain(|n| n != name);
    }
}

fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("app.json")
}

/// The saved preferences; a missing or unreadable file is the defaults.
pub fn load(host: &dyn Host) -> Settings {
    host.read(&path(host))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(host: &dyn Host, settings: &Settings) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(settings).unwrap_or_default();
    host.write_atomic(&path(host), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_json_from_before_automation_keeps_its_values_and_gets_the_defaults() {
        let old = br#"{"language":"vi","dismissedTips":["codex"]}"#;
        let settings: Settings = serde_json::from_slice(old).unwrap();
        assert_eq!(settings.language.as_deref(), Some("vi"));
        assert_eq!(settings.dismissed_tips, vec!["codex".to_string()]);
        assert!(!settings.auto_switch);
        assert_eq!(settings.auto_switch_at, 90);
        assert!(settings.notify_run_out);
        assert!(settings.warm_up.is_empty());
        assert_eq!(settings.warm_from, "07:00");
        assert_eq!(settings.warm_to, "23:00");
        assert!(settings.check_updates);
    }

    #[test]
    fn automation_settings_survive_a_save_and_load() {
        let settings = Settings {
            auto_switch: true,
            auto_switch_at: 80,
            notify_run_out: false,
            warm_up: vec!["ben2".into()],
            warm_from: "06:30".into(),
            warm_to: "01:00".into(),
            ..Settings::default()
        };
        let json = serde_json::to_vec(&settings).unwrap();
        assert_eq!(serde_json::from_slice::<Settings>(&json).unwrap(), settings);
    }

    #[test]
    fn warm_up_follows_a_rename_and_goes_with_a_forget() {
        let mut settings = Settings {
            warm_up: vec!["ben1".into(), "ben2".into()],
            ..Settings::default()
        };
        settings.rename_account("ben2", "work");
        assert_eq!(
            settings.warm_up,
            vec!["ben1".to_string(), "work".to_string()]
        );
        settings.forget_account("ben1");
        assert_eq!(settings.warm_up, vec!["work".to_string()]);
    }
}
