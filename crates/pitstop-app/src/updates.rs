//! Updates: a newer Piaz Switch is looked for in the GitHub release's
//! `latest.json`, and installed only when asked. The updater checks every
//! download against the public key in tauri.conf.json before it runs, so a
//! file changed on the way, or swapped in the release, is refused.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use pitstop_core::Host;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::app::{self, AppState};
use crate::i18n::Lang;
use crate::model::UiError;
use crate::settings;

/// The first look waits for the app to settle in; later ones come every six
/// hours, judged every quarter hour so a computer waking from sleep catches up.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(30);
const LOOK_EVERY: i64 = 6 * 60 * 60;
const TICK: Duration = Duration::from_secs(15 * 60);

/// A look that hangs gives up, so later ones still happen; a download gets
/// longer, but not forever, as the dialog waits on it.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Written just before the setup takes over, so the new version opens its
/// window even when the old one was started hidden, at login.
const SHOW_AFTER_UPDATE: &str = "show-after-update";

/// Where a copy that can't replace itself gets the new version by hand.
const RELEASES: &str = "https://github.com/Benpiaz/piaz-switch/releases/latest";

#[derive(Default)]
pub struct Updates(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    found: Option<Update>,
    checked_at: Option<i64>,
    /// The version a notification went out for: one per version.
    told: Option<String>,
    installing: bool,
    /// Counts the views handed out, so the window keeps only the latest.
    serial: u64,
}

/// What Settings and the update dialog show.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    /// Views can reach the window out of order; a lower one is older.
    pub serial: u64,
    pub current: &'static str,
    pub available: Option<Offer>,
    pub checked_at: Option<i64>,
    pub installing: bool,
    /// This copy can replace itself: installed by the setup on Windows, an
    /// app bundle on macOS. A portable copy is replaced by hand.
    pub can_install: bool,
    pub auto_check: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    pub version: String,
    /// The release's notes, from CHANGELOG.md.
    pub notes: Option<String>,
}

fn inner(app: &AppHandle) -> MutexGuard<'_, Inner> {
    let state = app.state::<Updates>().inner();
    state.0.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn view(app: &AppHandle) -> UpdateView {
    let auto_check = settings::load(&app::host()).check_updates;
    let mut inner = inner(app);
    inner.serial += 1;
    UpdateView {
        serial: inner.serial,
        current: env!("CARGO_PKG_VERSION"),
        available: inner.found.as_ref().map(|u| Offer {
            version: u.version.clone(),
            notes: u.body.clone().filter(|n| !n.trim().is_empty()),
        }),
        checked_at: inner.checked_at,
        installing: inner.installing,
        can_install: can_install(),
        auto_check,
    }
}

/// The newer version, for the tray menu.
pub fn available(app: &AppHandle) -> Option<String> {
    inner(app).found.as_ref().map(|u| u.version.clone())
}

/// Tell the window, and rebuild the tray menu, which may list the update;
/// off the calling thread, as a refresh reads files and the keychain.
fn changed(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("update", view(&app));
        app::refresh(&app);
    });
}

/// Whether this copy can put a new version in its own place.
fn can_install() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if cfg!(windows) {
        // The setup leaves its uninstaller beside the app; the portable zip
        // has none.
        exe.parent()
            .is_some_and(|dir| dir.join("uninstall.exe").is_file())
    } else if cfg!(target_os = "macos") {
        // Not still on the disk image, nor run from a quarantine copy.
        let path = exe.to_string_lossy();
        path.contains(".app/Contents/MacOS/")
            && !path.starts_with("/Volumes/")
            && !path.contains("/AppTranslocation/")
    } else {
        false
    }
}

/// Ask the release whether a newer version is out, and keep the answer.
async fn find(app: &AppHandle) -> Result<Option<String>, UiError> {
    let found = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(|e| UiError::new("updateCheck", e.to_string()))?
        .check()
        .await
        .map_err(|e| UiError::new("updateCheck", e.to_string()))?
        .map(|mut update| {
            update.timeout = Some(DOWNLOAD_TIMEOUT);
            update
        });
    let version = found.as_ref().map(|u| u.version.clone());
    {
        let mut inner = inner(app);
        inner.checked_at = Some(app::now());
        inner.found = found;
    }
    changed(app);
    Ok(version)
}

/// Look now and then while the app runs, if Settings allow it, and say once
/// per version that one is out.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_LOOK_AFTER);
        loop {
            if due(&app) {
                look(&app);
            }
            std::thread::sleep(TICK);
        }
    });
}

fn due(app: &AppHandle) -> bool {
    settings::load(&app::host()).check_updates
        && inner(app)
            .checked_at
            .is_none_or(|at| app::now() - at >= LOOK_EVERY)
}

/// One look in the background; a failure waits for the next tick.
fn look(app: &AppHandle) {
    let Ok(Some(version)) = tauri::async_runtime::block_on(find(app)) else {
        return;
    };
    {
        let mut inner = inner(app);
        if inner.told.as_deref() == Some(version.as_str()) {
            return;
        }
        inner.told = Some(version.clone());
    }
    let lang = Lang::resolve(settings::load(&app::host()).language.as_deref());
    let _ = app
        .notification()
        .builder()
        .title(lang.update_out(&version))
        .body(lang.update_out_why())
        .show();
}

#[tauri::command]
pub async fn update_status(app: AppHandle) -> UpdateView {
    tauri::async_runtime::spawn_blocking(move || view(&app))
        .await
        .expect("reading the update state panicked")
}

/// Look now, from Settings' Check for Updates.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<UpdateView, UiError> {
    // Seen here, so not notified again.
    if let Some(version) = find(&app).await? {
        inner(&app).told = Some(version);
    }
    Ok(view(&app))
}

#[tauri::command]
pub async fn set_check_updates(app: AppHandle, on: bool) -> Result<(), UiError> {
    tauri::async_runtime::spawn_blocking(move || {
        let host = app::host();
        let mut saved = settings::load(&host);
        saved.check_updates = on;
        settings::save(&host, &saved).map_err(|e| UiError::from(&e))?;
        changed(&app);
        if on && due(&app) {
            std::thread::spawn(move || look(&app));
        }
        Ok(())
    })
    .await
    .expect("saving the update setting panicked")
}

/// Download the update, check its signature, install it and start again.
/// Progress goes out as `update-progress` events. On Windows the setup takes
/// over and starts the new version itself; this call never returns there.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), UiError> {
    if !can_install() {
        // The setup would install a second copy beside a portable one.
        return Err(UiError::new(
            "updateManual",
            "This copy isn't installed; download the new version instead.",
        ));
    }
    let update = {
        let mut inner = inner(&app);
        if inner.installing {
            return Err(UiError::new(
                "updateBusy",
                "An update is already being installed.",
            ));
        }
        let update = inner
            .found
            .clone()
            .ok_or_else(|| UiError::new("noUpdate", "There is no update to install."))?;
        inner.installing = true;
        update
    };
    changed(&app);
    let result = download_and_install(&app, update).await;
    // Only a failure gets here on Windows; elsewhere success restarts below.
    inner(&app).installing = false;
    changed(&app);
    result?;
    app.restart()
}

async fn download_and_install(app: &AppHandle, update: Update) -> Result<(), UiError> {
    let progress = app.clone();
    let mut done: u64 = 0;
    let mut shown = None;
    let bytes = update
        .download(
            |chunk, total| {
                done += chunk as u64;
                // One event per percent, or per MiB when the size is unknown.
                let step = match total.filter(|t| *t > 0) {
                    Some(total) => done * 100 / total,
                    None => done >> 20,
                };
                if shown != Some(step) {
                    shown = Some(step);
                    let _ = progress.emit(
                        "update-progress",
                        serde_json::json!({ "done": done, "total": total }),
                    );
                }
            },
            || {},
        )
        .await
        .map_err(|e| UiError::new("updateDownload", e.to_string()))?;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // No switch is left half done, and no sign-in left running, when the
        // app goes.
        let state = app.state::<AppState>();
        let _ops = state.ops.lock();
        crate::signin::cancel(&app);
        let host = app::host();
        let marker = marker(&host);
        let _ = host.write_atomic(&marker, b"");
        let installed = update.install(bytes);
        if installed.is_err() {
            let _ = host.remove_file(&marker);
        }
        installed
    })
    .await
    .map_err(|e| UiError::new("updateInstall", e.to_string()))?
    .map_err(|e| UiError::new("updateInstall", e.to_string()))
}

fn marker(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join(SHOW_AFTER_UPDATE)
}

/// This start follows an update the user asked for: show the window even if
/// started hidden. Answers once.
pub fn just_updated() -> bool {
    let host = app::host();
    let marker = marker(&host);
    let there = host.read(&marker).ok().flatten().is_some();
    if there {
        let _ = host.remove_file(&marker);
    }
    there
}

/// The release page, for a copy that can't replace itself.
#[tauri::command]
pub fn open_releases(app: AppHandle) -> Result<(), UiError> {
    app.opener()
        .open_url(RELEASES, None::<&str>)
        .map_err(|e| UiError::new("open", e.to_string()))
}
