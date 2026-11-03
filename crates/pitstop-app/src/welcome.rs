//! The welcome sheet, shown once to someone who has just installed Piaz
//! Switch: what it does, the tools it found, where its icon lives, and
//! whether it opens at login. Someone with accounts already is past it.

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::app;
use crate::model::UiError;
use crate::settings;

/// Whether to show the welcome sheet, and whether to remember it as seen:
/// never again once seen, and never to someone with an account already.
pub fn judge(welcomed: bool, accounts: usize) -> (bool, bool) {
    match (welcomed, accounts) {
        (true, _) => (false, false),
        (false, 0) => (true, false),
        (false, _) => (false, true),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("a blocking task panicked")
}

/// Whether the window opens on the welcome sheet.
#[tauri::command]
pub async fn get_welcome() -> bool {
    blocking(|| {
        let host = app::host();
        let mut saved = settings::load(&host);
        // Accounts that can't be read are no newcomer's: say nothing.
        let Ok(state) = app::engine().state() else {
            return false;
        };
        let (show, mark) = judge(saved.welcomed, state.accounts.len());
        if mark {
            saved.welcomed = true;
            let _ = settings::save(&host, &saved);
        }
        show
    })
    .await
}

/// The welcome sheet was seen: closed, or Get Started.
#[tauri::command]
pub async fn set_welcomed() -> Result<(), UiError> {
    blocking(|| {
        let host = app::host();
        let mut saved = settings::load(&host);
        if saved.welcomed {
            return Ok(());
        }
        saved.welcomed = true;
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))
    })
    .await
}

/// Windows' taskbar settings, where the tray icon is kept in sight.
#[tauri::command]
pub async fn open_taskbar_settings(app: AppHandle) -> Result<(), UiError> {
    blocking(move || {
        if cfg!(not(windows)) {
            return Ok(());
        }
        app.opener()
            .open_url("ms-settings:taskbar", None::<&str>)
            .map_err(|e| UiError::new("open", e.to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_newcomer_is_welcomed_and_only_once() {
        assert_eq!(judge(false, 0), (true, false));
        assert_eq!(judge(true, 0), (false, false));
        // Someone with accounts already, from before the sheet: never shown.
        assert_eq!(judge(false, 3), (false, true));
        assert_eq!(judge(true, 3), (false, false));
    }
}
