//! What the window can ask of the app. Every answer is names, emails, plans,
//! usage or an error: never a token. Anything that touches files, the
//! keychain, the network or a lock runs off the UI thread.

use pitstop_core::engine::{RemoveOutcome, SwitchOutcome};
use pitstop_core::ProviderId;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::app::{self, AppState};
use crate::model::{Snapshot, UiError};
use crate::settings;

type Answer<T> = Result<T, UiError>;

fn provider(id: &str) -> Answer<ProviderId> {
    ProviderId::parse(id).map_err(|e| UiError::from(&e))
}

/// Run blocking work (files, keychain, network, locks) off the async runtime.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("a blocking task panicked")
}

#[tauri::command]
pub async fn get_snapshot(app: AppHandle) -> Option<Snapshot> {
    blocking(move || app::snapshot(&app)).await
}

/// Read the accounts again; with `usage`, ask for usage too.
#[tauri::command]
pub async fn refresh(app: AppHandle, usage: bool) {
    blocking(move || {
        app::refresh(&app);
        if usage {
            app::read_usage(&app);
        }
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Switched {
    pub switched: bool,
    /// Nothing happened: the tool holds a login Pitstop does not manage, and
    /// switching would replace it. Ask, then call again with
    /// `replace_unmanaged`.
    pub needs_confirmation: bool,
    pub provider: &'static str,
    pub name: String,
    /// The tool was signed in to someone Pitstop does not manage: that login
    /// was replaced, not parked.
    pub replaced: bool,
    pub replaced_label: Option<String>,
}

/// Switch `pid` to `name`. Unless `replace_unmanaged`, a login no enrolled
/// account owns stops the switch, decided on the same read of the login the
/// switch would park.
pub fn switch(
    app: &AppHandle,
    pid: ProviderId,
    name: &str,
    replace_unmanaged: bool,
) -> Answer<Switched> {
    if app::warming(app, pid, name) {
        return Err(UiError {
            code: "warmingUp",
            message: format!("{name} is warming up; switch when it ends"),
            name: Some(name.to_owned()),
        });
    }
    let state = app.state::<AppState>();
    let outcome = {
        let _op = state.ops.lock();
        let engine = app::engine();
        if replace_unmanaged {
            engine.switch(name, Some(pid))
        } else {
            engine.switch_keeping_unmanaged(name, Some(pid))
        }
    };
    app::refresh(app);
    let outcome = outcome.map_err(|e| UiError::from(&e))?;
    let unchanged = |needs_confirmation, replaced_label| Switched {
        switched: false,
        needs_confirmation,
        provider: pid.as_str(),
        name: name.to_owned(),
        replaced: false,
        replaced_label,
    };
    Ok(match outcome {
        SwitchOutcome::AlreadyActive => unchanged(false, None),
        SwitchOutcome::WouldReplace(login) => unchanged(true, login.label),
        SwitchOutcome::Switched {
            provider,
            name,
            replaced,
        } => {
            app::read_usage(app);
            Switched {
                switched: true,
                needs_confirmation: false,
                provider: provider.as_str(),
                name,
                replaced: replaced.is_some(),
                replaced_label: replaced.and_then(|r| r.label),
            }
        }
    })
}

#[tauri::command]
pub async fn use_account(
    app: AppHandle,
    provider: String,
    name: String,
    replace_unmanaged: Option<bool>,
) -> Answer<Switched> {
    let pid = self::provider(&provider)?;
    blocking(move || switch(&app, pid, &name, replace_unmanaged.unwrap_or(false))).await
}

/// Enroll the login the tool is signed in to, under `name`.
#[tauri::command]
pub async fn name_account(app: AppHandle, provider: String, name: String) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let enrolled = {
            let _op = state.ops.lock();
            app::engine().enroll(pid, &name)
        };
        app::refresh(&app);
        enrolled.map_err(|e| UiError::from(&e))?;
        app::read_usage_now(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn rename_account(
    app: AppHandle,
    provider: String,
    name: String,
    new_name: String,
) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let renamed = {
            let _op = state.ops.lock();
            app::engine().rename(&name, Some(pid), &new_name)
        };
        if let Ok(account) = &renamed {
            // The usage read under the old name belongs to the new one, and so
            // do its warm-up setting and record.
            app::rename_usage(&app, pid, &name, &account.name);
            if pid == ProviderId::ClaudeCode {
                crate::warm::follow_rename(&app::host(), &name, &account.name);
            }
        }
        app::refresh(&app);
        renamed.map(|_| ()).map_err(|e| UiError::from(&e))
    })
    .await
}

/// Forget an account; `true` when it was the one in use, whose login the
/// tool keeps.
#[tauri::command]
pub async fn forget_account(app: AppHandle, provider: String, name: String) -> Answer<bool> {
    let pid = self::provider(&provider)?;
    blocking(move || {
        let state = app.state::<AppState>();
        let removed = {
            let _op = state.ops.lock();
            app::engine().remove(&name, Some(pid))
        };
        if removed.is_ok() && pid == ProviderId::ClaudeCode {
            crate::warm::follow_forget(&app::host(), &name);
        }
        app::refresh(&app);
        removed
            .map(|outcome| outcome == RemoveOutcome::RemovedActive)
            .map_err(|e| UiError::from(&e))
    })
    .await
}

#[tauri::command]
pub async fn start_sign_in(
    app: AppHandle,
    provider: String,
    name: String,
    again: bool,
) -> Answer<()> {
    let pid = self::provider(&provider)?;
    blocking(move || crate::signin::start(&app, pid, name, again)).await
}

#[tauri::command]
pub async fn send_sign_in_code(app: AppHandle, code: String) -> Answer<()> {
    blocking(move || {
        let state = app.state::<AppState>();
        let job = state.sign_in.lock().map_err(|_| no_sign_in())?;
        job.as_ref().ok_or_else(no_sign_in)?.send_code(&code)
    })
    .await
}

#[tauri::command]
pub async fn cancel_sign_in(app: AppHandle) {
    blocking(move || crate::signin::cancel(&app)).await
}

/// Open the sign-in page the tool printed. The window never hands over an
/// address of its own: only the one read from the tool is opened.
#[tauri::command]
pub async fn open_sign_in_page(app: AppHandle) -> Answer<()> {
    blocking(move || {
        let url = {
            let state = app.state::<AppState>();
            let job = state.sign_in.lock().map_err(|_| no_sign_in())?;
            job.as_ref().and_then(|j| j.url())
        }
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(no_sign_in)?;
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| UiError::new("open", e.to_string()))
    })
    .await
}

fn no_sign_in() -> UiError {
    UiError::new("signIn", "No sign-in is running.")
}

/// `en`, `vi`, or none to follow the system.
#[tauri::command]
pub async fn set_language(app: AppHandle, language: Option<String>) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.language = language.filter(|l| l == "en" || l == "vi");
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        Ok(())
    })
    .await
}

/// `light`, `dark`, or none to follow the system.
#[tauri::command]
pub async fn set_theme(app: AppHandle, theme: Option<String>) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.theme = theme.filter(|t| t == "light" || t == "dark");
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::apply_window_theme(&app, saved.theme.as_deref());
        app::refresh(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_autostart(app: AppHandle, on: bool) -> Answer<()> {
    blocking(move || {
        let autolaunch = app.autolaunch();
        let changed = if on {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        app::refresh(&app);
        changed.map_err(|e| UiError::new("autostart", e.to_string()))
    })
    .await
}

#[tauri::command]
pub async fn dismiss_tip(app: AppHandle, provider: String) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        if !saved.dismissed_tips.contains(&provider) {
            saved.dismissed_tips.push(provider);
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn quit(app: AppHandle) {
    blocking(move || app::quit(&app)).await
}

/// Automatic switch, run-out notices and the warm-up span; what is left out
/// stays as it is.
#[tauri::command]
pub async fn set_automation(
    app: AppHandle,
    auto_switch: Option<bool>,
    auto_switch_at: Option<u8>,
    notify_run_out: Option<bool>,
    warm_from: Option<String>,
    warm_to: Option<String>,
) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        if let Some(on) = auto_switch {
            saved.auto_switch = on;
        }
        if let Some(at) = auto_switch_at {
            saved.auto_switch_at = at.clamp(50, 100);
        }
        if let Some(on) = notify_run_out {
            saved.notify_run_out = on;
        }
        let clock = |c: &String| crate::duty::is_clock(c);
        if let Some(from) = warm_from.filter(clock) {
            saved.warm_from = from;
        }
        if let Some(to) = warm_to.filter(clock) {
            saved.warm_to = to;
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        app::run_duties(&app);
        Ok(())
    })
    .await
}

/// Turn warm-ups on or off for one Claude Code account.
#[tauri::command]
pub async fn set_warm_up(app: AppHandle, name: String, on: bool) -> Answer<()> {
    blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.warm_up.retain(|n| n != &name);
        if on {
            saved.warm_up.push(name);
        }
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        app::refresh(&app);
        app::run_duties(&app);
        Ok(())
    })
    .await
}
