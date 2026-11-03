//! Notifications with a button: Switch to an account, Switch back, Sign in
//! again. The notification plugin shows them (as Windows toasts, with the
//! same app identity as every other notification of the app) and reports a
//! click on a button, or on the notification itself, while the app runs.
//!
//! What a click does travels as a short string, an [`Act`]: the button's
//! action id, or the notification's `tap` extra for a click on its body.
//! On Windows a click is only reported while the toast is on screen, not
//! once it moved to the Action Center; there it does nothing.

use pitstop_core::ProviderId;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::{Action, ActionType, NotificationExt};

use crate::app;

/// What a click on a notification, or on its button, does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Bring the window up.
    Open,
    /// Switch `provider` to `name`.
    Switch { provider: ProviderId, name: String },
    /// Switch `provider` back to `to`, but only while `from` (the account
    /// the app switched to) is still the one in use.
    SwitchBack {
        provider: ProviderId,
        from: String,
        to: String,
    },
    /// Open Sign In Again for `name`.
    SignInAgain { provider: ProviderId, name: String },
}

impl Act {
    /// `open`, `switch|codex|main`, `back|claude-code|ben2|ben1`,
    /// `signin|claude-code|ben3`. Account names hold no `|`.
    pub fn encode(&self) -> String {
        match self {
            Act::Open => "open".into(),
            Act::Switch { provider, name } => format!("switch|{}|{name}", provider.as_str()),
            Act::SwitchBack { provider, from, to } => {
                format!("back|{}|{from}|{to}", provider.as_str())
            }
            Act::SignInAgain { provider, name } => format!("signin|{}|{name}", provider.as_str()),
        }
    }

    pub fn decode(text: &str) -> Option<Act> {
        let parts: Vec<&str> = text.split('|').collect();
        let provider = |id: &str| ProviderId::parse(id).ok();
        let named = |name: &&str| !name.is_empty();
        Some(match parts.as_slice() {
            ["open"] => Act::Open,
            ["switch", p, name] if named(name) => Act::Switch {
                provider: provider(p)?,
                name: (*name).into(),
            },
            ["back", p, from, to] if named(from) && named(to) => Act::SwitchBack {
                provider: provider(p)?,
                from: (*from).into(),
                to: (*to).into(),
            },
            ["signin", p, name] if named(name) => Act::SignInAgain {
                provider: provider(p)?,
                name: (*name).into(),
            },
            _ => return None,
        })
    }
}

/// The action reported for a click on the notification itself.
const TAP: &str = "tap";

/// Have clicks on notifications, and on their buttons, do what they say.
/// Called once at start, before anything is posted: a notification shown
/// before then reports nothing.
pub fn listen(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.notification().on_action(move |performed| {
        let act = if performed.action_id() == TAP {
            performed
                .notification()
                .and_then(|n| n.extra().get(TAP))
                .and_then(|v| v.as_str())
                .and_then(Act::decode)
                .unwrap_or(Act::Open)
        } else {
            Act::decode(performed.action_id()).unwrap_or(Act::Open)
        };
        // Off the notification's own thread: a switch can take a while.
        let app = handle.clone();
        std::thread::spawn(move || run(&app, act));
    });
}

/// Post a notification; a click on it does `tap`, and `button`, when there
/// is one, adds a button with its words and what it does.
pub fn post(app: &AppHandle, title: &str, body: &str, button: Option<(&str, Act)>, tap: Act) {
    let notification = app.notification();
    let mut builder = notification
        .builder()
        .title(title)
        .body(body)
        .extra(TAP, tap.encode());
    if let Some((label, act)) = button {
        let id = act.encode();
        // One action type per button: its id is what the click reports, and
        // registering it again only replaces it.
        let kind = format!("piaz:{id}");
        let registered = notification.register_action_types(vec![ActionType::builder(&kind)
            .actions(vec![Action::builder(&id, label).build()])
            .build()]);
        if registered.is_ok() {
            builder = builder.action_type_id(kind);
        }
    }
    let _ = builder.show();
}

fn run(app: &AppHandle, act: Act) {
    match act {
        Act::Open => app::show_window(app),
        Act::Switch { provider, name } => switch(app, provider, &name),
        Act::SwitchBack { provider, from, to } => {
            // Only undo the switch the notification is about: once the user
            // moved on, the button does nothing but show the window.
            if app::in_use_name(provider).as_deref() == Some(from.as_str()) {
                switch(app, provider, &to);
            } else {
                app::show_window(app);
            }
        }
        Act::SignInAgain { provider, name } => {
            app::open_dialog(app, "sign-in-again", Some(provider.as_str()), Some(&name));
        }
    }
}

/// A switch asked for from a notification: as from the tray menu, a
/// question or a failure goes to the window.
fn switch(app: &AppHandle, provider: ProviderId, name: &str) {
    match crate::commands::switch_by_hand(app, provider, name, false, false) {
        Ok(asked) if asked.needs_confirmation || asked.needs_restart => {
            app::show_window(app);
            let _ = app.emit(
                "open-dialog",
                serde_json::json!({
                    "kind": if asked.needs_restart { "confirm-restart" } else { "confirm-switch" },
                    "provider": provider.as_str(),
                    "name": name,
                    "label": asked.replaced_label,
                }),
            );
        }
        Ok(switched) => {
            let _ = app.emit("switched", &switched);
        }
        Err(error) => {
            app::show_window(app);
            let _ = app.emit(
                "switch-failed",
                serde_json::json!({ "name": name, "error": error }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_act_reads_back_as_itself() {
        let acts = [
            Act::Open,
            Act::Switch {
                provider: ProviderId::Codex,
                name: "main".into(),
            },
            Act::SwitchBack {
                provider: ProviderId::ClaudeCode,
                from: "ben2".into(),
                to: "ben1".into(),
            },
            Act::SignInAgain {
                provider: ProviderId::Cursor,
                name: "design_2".into(),
            },
        ];
        for act in acts {
            assert_eq!(Act::decode(&act.encode()), Some(act.clone()), "{act:?}");
        }
        assert_eq!(
            Act::Switch {
                provider: ProviderId::Codex,
                name: "main".into()
            }
            .encode(),
            "switch|codex|main"
        );
    }

    #[test]
    fn anything_else_reads_as_nothing() {
        for text in [
            "",
            "tap",
            "switch|codex",
            "switch|codex|",
            "switch|nope|main",
            "back|claude-code|ben2",
            "signin|claude-code|a|b",
            "open|x",
        ] {
            assert_eq!(Act::decode(text), None, "{text}");
        }
    }
}
