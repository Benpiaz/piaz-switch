//! The tray / menu bar item and its native menu, rebuilt from each snapshot.

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Wry};

use crate::app;
use crate::i18n::Lang;
use crate::model::{self, MenuLine, Snapshot};

const TRAY_ID: &str = "pitstop";

/// macOS colors a template image itself; Windows shows the orange mark.
fn icon() -> Image<'static> {
    let bytes: &[u8] = if cfg!(target_os = "macos") {
        include_bytes!("../icons/tray/tray-template.png")
    } else {
        include_bytes!("../icons/tray/tray.png")
    };
    Image::from_bytes(bytes).expect("the tray icon is a valid PNG")
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::new(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon())
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Piaz Switch")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| chosen(app, event.id().as_ref()))
        .build(app)?;
    Ok(())
}

/// The local time of day for "Updated 23:58".
fn clock(at: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(at, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

pub fn update(app: &AppHandle, snapshot: &Snapshot, lang: Lang) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let mut lines = model::menu(snapshot, lang, &clock);
    if let Some(version) = crate::updates::available(app) {
        lines.splice(
            0..0,
            [
                MenuLine::Action {
                    id: "update".into(),
                    title: lang.update_to(&version),
                    enabled: true,
                },
                MenuLine::Separator,
            ],
        );
    }
    if let Ok(menu) = build(app, &lines) {
        let _ = tray.set_menu(Some(menu));
    }
    let title = model::tray_title(snapshot);
    #[cfg(target_os = "macos")]
    let _ = tray.set_title(title.as_deref());
    let _ = tray.set_tooltip(Some(match &title {
        Some(title) => format!("Piaz Switch — {title}"),
        None => "Piaz Switch".to_owned(),
    }));
}

fn build(app: &AppHandle, lines: &[MenuLine]) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    for (n, line) in lines.iter().enumerate() {
        match line {
            MenuLine::Heading(text) => menu.append(&MenuItem::with_id(
                app,
                format!("text|{n}"),
                text,
                false,
                None::<&str>,
            )?)?,
            MenuLine::Account { id, title, checked } => menu.append(&CheckMenuItem::with_id(
                app,
                id.as_str(),
                title,
                true,
                *checked,
                None::<&str>,
            )?)?,
            MenuLine::Note(text) => menu.append(&MenuItem::with_id(
                app,
                format!("text|{n}"),
                format!("    {text}"),
                false,
                None::<&str>,
            )?)?,
            MenuLine::Action { id, title, enabled } => menu.append(&MenuItem::with_id(
                app,
                id.as_str(),
                title,
                *enabled,
                None::<&str>,
            )?)?,
            MenuLine::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
        }
    }
    Ok(menu)
}

/// Runs on the UI thread: anything slower than showing a window goes to a
/// thread of its own.
fn chosen(app: &AppHandle, id: &str) {
    let parts: Vec<&str> = id.split('|').collect();
    let owned = app.clone();
    match parts.as_slice() {
        ["use" | "suggest", provider, name] => switch(app, provider, name),
        ["name", provider] => app::open_dialog(app, "name", Some(provider), None),
        ["add"] => app::open_dialog(app, "add", None, None),
        ["update"] => app::open_dialog(app, "update", None, None),
        ["refresh"] => {
            std::thread::spawn(move || app::read_usage(&owned));
        }
        ["open"] => app::show_window(app),
        ["quit"] => {
            std::thread::spawn(move || app::quit(&owned));
            return;
        }
        _ => {}
    }
    // A check item toggles itself when chosen; put the menu back as it is.
    let app = app.clone();
    std::thread::spawn(move || app::refresh(&app));
}

/// Switch from the menu. Whether the tool holds a login Pitstop does not
/// manage is decided by the switch itself, on the login it reads; if so, the
/// question goes to the window instead, and so does a failure.
fn switch(app: &AppHandle, provider: &str, name: &str) {
    let Ok(pid) = pitstop_core::ProviderId::parse(provider) else {
        return;
    };
    let app = app.clone();
    let name = name.to_owned();
    std::thread::spawn(
        move || match crate::commands::switch(&app, pid, &name, false) {
            Ok(asked) if asked.needs_confirmation => {
                app::show_window(&app);
                let _ = app.emit(
                    "open-dialog",
                    serde_json::json!({
                        "kind": "confirm-switch",
                        "provider": pid.as_str(),
                        "name": name,
                        "label": asked.replaced_label,
                    }),
                );
            }
            Ok(switched) => {
                let _ = app.emit("switched", &switched);
            }
            Err(error) => {
                app::show_window(&app);
                let _ = app.emit(
                    "switch-failed",
                    serde_json::json!({ "name": name, "error": error }),
                );
            }
        },
    );
}
