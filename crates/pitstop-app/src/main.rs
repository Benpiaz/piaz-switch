//! Pitstop: a tray / menu bar app to switch Claude Code and Codex accounts,
//! with a window to manage them. All account work happens in pitstop-core.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod duty;
mod i18n;
mod links;
mod model;
mod settings;
mod signin;
mod spend;
mod tray;
mod updates;
mod usage_logs;
mod warm;

use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

fn main() {
    // Started with the session: tray only, no window. Not right after an
    // update, though, which starts the new version with the old one's flags.
    let updated = updates::just_updated();
    let hidden = std::env::args().any(|a| a == "--hidden") && !updated;

    tauri::Builder::default()
        // First: a second launch only brings the running one's window up.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            app::show_window(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app::AppState::default())
        .manage(usage_logs::UsageLogs::default())
        .manage(updates::Updates::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh,
            commands::use_account,
            commands::name_account,
            commands::rename_account,
            commands::forget_account,
            commands::start_sign_in,
            commands::send_sign_in_code,
            commands::cancel_sign_in,
            commands::open_sign_in_page,
            commands::set_language,
            commands::set_theme,
            commands::set_autostart,
            commands::dismiss_tip,
            commands::set_automation,
            commands::set_warm_up,
            commands::quit,
            usage_logs::usage_report,
            updates::update_status,
            updates::check_for_update,
            updates::set_check_updates,
            updates::install_update,
            updates::open_releases,
            links::open_link,
        ])
        .setup(move |tauri_app| {
            #[cfg(target_os = "macos")]
            tauri_app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let handle = tauri_app.handle().clone();
            tray::create(&handle)?;
            app::apply_window_theme(&handle, settings::load(&app::host()).theme.as_deref());
            app::refresh(&handle);
            app::start_background(&handle);
            usage_logs::refresh_prices(&handle);
            updates::start(&handle);
            if !hidden {
                app::show_window(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps Pitstop in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                app::hide_window(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Piaz Switch")
        .run(|_handle, _event: RunEvent| {
            #[cfg(target_os = "macos")]
            if let RunEvent::Reopen { .. } = _event {
                app::show_window(_handle);
            }
        });
}
