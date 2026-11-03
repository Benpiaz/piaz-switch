//! The Windows Jump List: right-clicking Piaz Switch on the taskbar, or where
//! it is pinned in Start, offers "Switch to <account>" for each account and
//! "Add Account". Each task starts this program with `--switch-to
//! <tool>|<name>` or `--add-account`; the running app is handed them by the
//! single-instance plugin, or a new one gets them at startup. Nothing here
//! runs on macOS.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use pitstop_core::ProviderId;
use tauri::{AppHandle, Emitter};

use crate::app;
use crate::i18n::Lang;
use crate::model::Snapshot;
use crate::settings;

const SWITCH_TO: &str = "--switch-to";
const ADD_ACCOUNT: &str = "--add-account";

/// What a Jump List task asks of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    SwitchTo { provider: ProviderId, name: String },
    AddAccount,
}

/// Whether `name` could be an account's name, as Piaz Switch allows them.
fn account_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn switch_to(value: &str) -> Option<Task> {
    let (provider, name) = value.split_once('|')?;
    let provider = ProviderId::parse(provider).ok()?;
    account_name(name).then(|| Task::SwitchTo {
        provider,
        name: name.to_owned(),
    })
}

/// The task a command line asks for, if any: `--switch-to <tool>|<name>`
/// (or `--switch-to=<tool>|<name>`) or `--add-account`.
pub fn task_from<S: AsRef<str>>(args: &[S]) -> Option<Task> {
    let mut args = args.iter().map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        if arg == ADD_ACCOUNT {
            return Some(Task::AddAccount);
        }
        if arg == SWITCH_TO {
            return args.next().and_then(switch_to);
        }
        if let Some(value) = arg.strip_prefix("--switch-to=") {
            return switch_to(value);
        }
    }
    None
}

/// One task as the Jump List shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTask {
    pub title: String,
    /// The command line it starts this program with.
    pub args: String,
}

/// The tasks, in the order of the tray menu: each account by tool, its tool
/// named once several tools have accounts, then Add Account.
pub fn tasks(snapshot: &Snapshot, lang: Lang) -> Vec<JumpTask> {
    let shown: Vec<_> = snapshot
        .tools
        .iter()
        .filter(|t| !t.accounts.is_empty())
        .collect();
    let several = shown.len() > 1;
    let mut tasks: Vec<JumpTask> = shown
        .iter()
        .flat_map(|tool| {
            tool.accounts.iter().map(move |account| JumpTask {
                title: if several {
                    lang.switch_to_on(&account.name, tool.name)
                } else {
                    lang.switch_to(&account.name)
                },
                args: format!("{SWITCH_TO} \"{}|{}\"", tool.id, account.name),
            })
        })
        .collect();
    tasks.push(JumpTask {
        title: lang.add_account_task().into(),
        args: ADD_ACCOUNT.into(),
    });
    tasks
}

/// The tasks last written, so the list is written only when they change.
/// Never held while the list is written.
static WRITTEN: Mutex<Option<Vec<JumpTask>>> = Mutex::new(None);
/// One writer of the list at a time; only writers' own threads take it.
static WRITING: Mutex<()> = Mutex::new(());

/// Write the Jump List again if the accounts (or the language) changed since
/// it was last written. Off the calling thread; nothing on macOS, and nothing
/// from a build run out of `target/`, so trying the app never touches the
/// installed one's list.
pub fn update(app: &AppHandle, snapshot: &Snapshot, lang: Lang) {
    if cfg!(not(windows)) || snapshot.error.is_some() || !installed_build() {
        return;
    }
    let tasks = tasks(snapshot, lang);
    if WRITTEN
        .lock()
        .map(|w| w.as_ref() == Some(&tasks))
        .unwrap_or(true)
    {
        return;
    }
    let app_id = app.config().identifier.clone();
    std::thread::spawn(move || {
        // One writer at a time; a newer list may have been written meanwhile.
        let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        if WRITTEN
            .lock()
            .map(|w| w.as_ref() == Some(&tasks))
            .unwrap_or(true)
        {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        if write(&app_id, &exe, &tasks).is_ok() {
            if let Ok(mut written) = WRITTEN.lock() {
                *written = Some(tasks);
            }
        }
    });
}

/// Whether this is a build people run (installed, or the portable zip),
/// not one run from a `target/debug` or `target/release` folder.
pub fn installed_build() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| !built_in(dir)))
        .unwrap_or(false)
}

/// Whether `dir` is a `target/debug` or `target/release` folder.
fn built_in(dir: &std::path::Path) -> bool {
    let tail: Vec<String> = dir
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    tail.len() == 2 && tail[1] == "target" && (tail[0] == "debug" || tail[0] == "release")
}

/// Name this process with the app's AppUserModelID, the one its Start menu
/// shortcut and notifications carry, so the taskbar groups its window with
/// the pinned icon and shows the Jump List there however it was started (at
/// sign-in, the Run entry starts the program directly). Call before any
/// window is made.
pub fn claim_app_id(app_id: &str) {
    #[cfg(windows)]
    if installed_build() {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        // SAFETY: a plain call taking a string the call copies.
        let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(app_id)) };
    }
    #[cfg(not(windows))]
    let _ = app_id;
}

/// Write `tasks` as the Jump List of `app_id`, each starting `exe`.
#[cfg(windows)]
pub fn write(app_id: &str, exe: &std::path::Path, tasks: &[JumpTask]) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
        SHStrDupW, ShellLink,
    };

    let exe = HSTRING::from(exe.as_os_str());
    // SAFETY: COM calls on objects made here and used on this thread only;
    // the title's string is handed to the PROPVARIANT, which frees it.
    unsafe {
        // This thread's own apartment, for the shell's objects.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let written = (|| {
            let list: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
            list.SetAppID(&HSTRING::from(app_id))?;
            let mut slots = 0u32;
            let _removed: IObjectArray = list.BeginList(&mut slots)?;
            let collection: IObjectCollection =
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
            for task in tasks {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&exe)?;
                link.SetArguments(&HSTRING::from(task.args.as_str()))?;
                link.SetIconLocation(&exe, 0)?;
                link.SetDescription(&HSTRING::from(task.title.as_str()))?;
                // A task shows its System.Title.
                let store: IPropertyStore = link.cast()?;
                let mut title = PROPVARIANT::default();
                (*title.Anonymous.Anonymous).vt = VT_LPWSTR;
                (*title.Anonymous.Anonymous).Anonymous.pwszVal =
                    SHStrDupW(&HSTRING::from(task.title.as_str()))?;
                let set = store
                    .SetValue(&PKEY_Title, &title)
                    .and_then(|()| store.Commit());
                let _ = PropVariantClear(&mut title);
                set?;
                collection.AddObject(&link)?;
            }
            let array: IObjectArray = collection.cast()?;
            list.AddUserTasks(&array)?;
            list.CommitList()
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        written
    }
}

#[cfg(not(windows))]
pub fn write(_app_id: &str, _exe: &std::path::Path, _tasks: &[JumpTask]) -> Result<(), ()> {
    Ok(())
}

// ---------- doing a task ----------

/// A dialog the window is asked to open, kept until the window takes it: a
/// task can come before the window's page has loaded.
static PENDING: Mutex<Option<serde_json::Value>> = Mutex::new(None);
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// Show the window with a dialog open: now if its page is listening, else
/// when it asks with [`take_pending_dialog`]. The serial lets the page open
/// it once whichever way it comes.
pub fn ask_window(app: &AppHandle, mut request: serde_json::Value) {
    request["serial"] = (SERIAL.fetch_add(1, Ordering::SeqCst) + 1).into();
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some(request.clone());
    }
    app::show_window(app);
    let _ = app.emit("open-dialog", request);
}

/// The dialog a task asked for before the window could hear it.
#[tauri::command]
pub fn take_pending_dialog() -> Option<serde_json::Value> {
    PENDING.lock().ok()?.take()
}

/// The window opened the dialog numbered `serial`, as it heard of it: it is
/// no longer pending, so a reload of the page doesn't open it again.
#[tauri::command]
pub fn dialog_opened(serial: u64) {
    if let Ok(mut pending) = PENDING.lock() {
        let done = pending
            .as_ref()
            .and_then(|p| p["serial"].as_u64())
            .is_some_and(|waiting| waiting <= serial);
        if done {
            *pending = None;
        }
    }
}

/// Do what a Jump List task asks. A switch opens no window: it asks only when
/// it has to (an unnamed login would be replaced, an editor has to restart),
/// and says why in a notification when it can't happen.
pub fn run(app: &AppHandle, task: Task) {
    match task {
        Task::AddAccount => ask_window(app, serde_json::json!({ "kind": "add" })),
        Task::SwitchTo { provider, name } => {
            let app = app.clone();
            std::thread::spawn(move || switch(&app, provider, &name));
        }
    }
}

fn switch(app: &AppHandle, provider: ProviderId, name: &str) {
    match crate::commands::switch_by_hand(app, provider, name, false, false) {
        Ok(asked) if asked.needs_confirmation || asked.needs_restart => ask_window(
            app,
            serde_json::json!({
                "kind": if asked.needs_restart { "confirm-restart" } else { "confirm-switch" },
                "provider": provider.as_str(),
                "name": name,
                "label": asked.replaced_label,
            }),
        ),
        Ok(switched) => {
            let _ = app.emit("switched", &switched);
        }
        Err(error) => {
            let lang = Lang::resolve(settings::load(&app::host()).language.as_deref());
            app::notify(
                app,
                &lang.couldnt_switch(name),
                &lang.switch_failed_why(error.code, name, provider.display_name(), &error.message),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountView, LiveView, ToolView};

    #[test]
    fn a_task_is_read_from_the_command_line_it_starts() {
        let switch = |provider, name: &str| {
            Some(Task::SwitchTo {
                provider,
                name: name.into(),
            })
        };
        assert_eq!(
            task_from(&["pitstop-app.exe", "--switch-to", "claude-code|ben2"]),
            switch(ProviderId::ClaudeCode, "ben2")
        );
        assert_eq!(
            task_from(&["x", "--switch-to=codex|main_1"]),
            switch(ProviderId::Codex, "main_1")
        );
        assert_eq!(task_from(&["x", "--add-account"]), Some(Task::AddAccount));
        assert_eq!(task_from(&["x", "--hidden"]), None);
        assert_eq!(task_from::<&str>(&[]), None);
    }

    #[test]
    fn a_task_naming_no_known_tool_or_no_proper_name_is_nothing() {
        for bad in [
            vec!["x", "--switch-to"],
            vec!["x", "--switch-to", "claude-code"],
            vec!["x", "--switch-to", "nope|ben2"],
            vec!["x", "--switch-to", "claude-code|"],
            vec!["x", "--switch-to", "claude-code|me@example.com"],
            vec!["x", "--switch-to", "claude-code|..\\..\\x"],
        ] {
            assert_eq!(task_from(&bad), None, "{bad:?}");
        }
    }

    fn account(name: &str) -> AccountView {
        AccountView {
            name: name.into(),
            label: None,
            plan: None,
            in_use: false,
            parked_at: None,
            usage: None,
        }
    }

    fn tool(id: &'static str, name: &'static str, accounts: &[&str]) -> ToolView {
        ToolView {
            id,
            name,
            installed: Some(true),
            live: LiveView::LoggedOut,
            accounts: accounts.iter().map(|a| account(a)).collect(),
        }
    }

    fn snapshot(tools: Vec<ToolView>) -> Snapshot {
        Snapshot {
            revision: 1,
            now: 0,
            reading: false,
            updated_at: None,
            error: None,
            tools,
            suggestions: Vec::new(),
            language: "en",
            language_choice: None,
            theme_choice: None,
            autostart: false,
            dismissed_tips: Vec::new(),
            version: "0",
            automation: crate::model::Automation {
                auto_switch: false,
                auto_switch_at: 90,
                use_first: crate::duty::UseFirst::MostLeft,
                notify_run_out: true,
                notify_near: true,
                warm_from: "07:00".into(),
                warm_to: "23:00".into(),
                warm_up: Vec::new(),
            },
        }
    }

    #[test]
    fn each_account_gets_a_task_then_add_account() {
        let one = snapshot(vec![
            tool("claude-code", "Claude Code", &["work", "personal"]),
            tool("codex", "Codex", &[]),
        ]);
        let titles: Vec<String> = tasks(&one, Lang::En).into_iter().map(|t| t.title).collect();
        assert_eq!(
            titles,
            ["Switch to work", "Switch to personal", "Add Account"]
        );
        assert_eq!(
            tasks(&one, Lang::En)[0].args,
            "--switch-to \"claude-code|work\""
        );
        // What a task starts with is read back as that task.
        let args = tasks(&one, Lang::En)[1].args.replace('"', "");
        let parts: Vec<&str> = args.split(' ').collect();
        assert_eq!(
            task_from(&parts),
            Some(Task::SwitchTo {
                provider: ProviderId::ClaudeCode,
                name: "personal".into()
            })
        );

        let two = snapshot(vec![
            tool("claude-code", "Claude Code", &["work"]),
            tool("codex", "Codex", &["main"]),
        ]);
        let titles: Vec<String> = tasks(&two, Lang::Vi).into_iter().map(|t| t.title).collect();
        assert_eq!(
            titles,
            [
                "Chuyển sang work (Claude Code)",
                "Chuyển sang main (Codex)",
                "Thêm tài khoản"
            ]
        );
    }

    #[test]
    fn a_build_run_from_target_is_not_one_people_run() {
        let built = |p: &str| built_in(std::path::Path::new(p));
        assert!(built("C:/src/pitstop/target/release"));
        assert!(built("/src/pitstop/target/Debug"));
        assert!(!built("/src/pitstop/target/debug/deps"));
        assert!(!built("C:/Users/x/AppData/Local/Piaz Switch"));
    }
}
