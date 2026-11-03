//! Whether the app is running, told by a lock it holds on
//! `~/.pitstop/app.lock` for as long as it runs. The system lets the lock go
//! when the app stops, however it stops, so a crash leaves nothing behind.
//!
//! The daily renewal looks at it: the app renews parked logins before every
//! read, and two renewers of one login would race on its refresh token,
//! which each renewal replaces.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// The lock, held until dropped.
#[derive(Debug)]
pub struct Presence {
    _file: File,
}

/// What trying the lock found.
#[derive(Debug)]
pub enum Claim {
    /// Nobody else holds it: now this process does.
    Held(Presence),
    /// Another process holds it: the app runs (or a renewal does).
    Busy,
    /// It couldn't be tried: no folder, no file.
    Unknown,
}

pub fn lock_path(home: &Path) -> PathBuf {
    home.join(".pitstop").join("app.lock")
}

/// Take the lock under `home` if nobody holds it.
pub fn claim(home: &Path) -> Claim {
    let path = lock_path(home);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return Claim::Unknown;
        }
    }
    let Ok(file) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    else {
        return Claim::Unknown;
    };
    match file.try_lock() {
        Ok(()) => Claim::Held(Presence { _file: file }),
        Err(TryLockError::WouldBlock) => Claim::Busy,
        Err(TryLockError::Error(_)) => Claim::Unknown,
    }
}

/// Whether an image name is the app's program: the installed
/// `pitstop-app(.exe)`, or any `Piaz Switch….exe` (the portable copy, a
/// preview build).
fn app_program(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    let name = name.rsplit(['/', '\\']).next().unwrap_or(&name);
    name == "pitstop-app"
        || name == "pitstop-app.exe"
        || (name.starts_with("piaz switch") && name.ends_with(".exe"))
}

/// Whether Windows' `tasklist /FO CSV /NH` lists the app's program running
/// under a process other than `me`.
pub fn app_in_tasklist(csv: &str, me: u32) -> bool {
    csv.lines().any(|line| {
        let mut fields = line.split("\",\"").map(|f| f.trim_matches('"'));
        let (Some(image), Some(pid)) = (fields.next(), fields.next()) else {
            return false;
        };
        app_program(image) && pid.trim().parse::<u32>().is_ok_and(|pid| pid != me)
    })
}

/// Whether `ps -axo pid=,comm=` (macOS) lists the app's program running
/// under a process other than `me`.
pub fn app_in_ps(listing: &str, me: u32) -> bool {
    listing.lines().any(|line| {
        let line = line.trim_start();
        let Some((pid, program)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        let program = program.trim();
        let ours = program.contains("/Piaz Switch.app/Contents/MacOS/") || app_program(program);
        ours && pid.parse::<u32>().is_ok_and(|pid| pid != me)
    })
}

/// Whether the app's program runs in another process, by name: what tells
/// apart a version of the app from before the lock, which holds none. The
/// renewal running as the app's own program (`pitstop-app renew`) is this
/// process and doesn't count. Can't tell: none found.
pub fn app_process_running() -> bool {
    let me = std::process::id();
    let listing = |program: &str, args: &[&str]| {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if cfg!(windows) {
        listing("tasklist", &["/FO", "CSV", "/NH"]).is_some_and(|csv| app_in_tasklist(&csv, me))
    } else if cfg!(target_os = "macos") {
        listing("ps", &["-axo", "pid=,comm="]).is_some_and(|ps| app_in_ps(&ps, me))
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_is_found_by_any_of_its_program_names_but_not_this_process() {
        let csv = concat!(
            "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\n",
            "\"claude.exe\",\"4120\",\"Console\",\"1\",\"90,000 K\"\n",
        );
        assert!(!app_in_tasklist(csv, 1));
        for image in [
            "pitstop-app.exe",
            "Piaz Switch.exe",
            "Piaz Switch preview.exe",
            "PITSTOP-APP.EXE",
        ] {
            let listed = format!("{csv}\"{image}\",\"10524\",\"Console\",\"1\",\"120,000 K\"\n");
            assert!(app_in_tasklist(&listed, 1), "{image}");
            // The renewal itself, run as the app's program.
            assert!(!app_in_tasklist(&listed, 10524), "{image}");
        }
        assert!(!app_in_tasklist(
            "\"piaz-switch.exe\",\"77\",\"Console\",\"1\",\"5 K\"",
            1
        ));
        assert!(!app_in_tasklist("INFO: No tasks are running", 1));

        let ps =
            "  1 /sbin/launchd\n 812 /Applications/Piaz Switch.app/Contents/MacOS/pitstop-app\n";
        assert!(app_in_ps(ps, 1));
        assert!(!app_in_ps(ps, 812));
        assert!(!app_in_ps(
            "  1 /sbin/launchd\n 90 /usr/local/bin/piaz-switch\n",
            1
        ));
    }

    #[test]
    fn a_held_lock_says_the_app_runs_until_it_lets_go() {
        let home = tempfile::tempdir().unwrap();
        let app = match claim(home.path()) {
            Claim::Held(held) => held,
            other => panic!("expected the lock, got {other:?}"),
        };
        assert!(matches!(claim(home.path()), Claim::Busy));
        drop(app);
        assert!(matches!(claim(home.path()), Claim::Held(_)));
    }

    #[test]
    fn a_home_that_cant_hold_the_folder_is_unknown() {
        let home = tempfile::tempdir().unwrap();
        // `.pitstop` is a file here, so the folder can't be made.
        std::fs::write(home.path().join(".pitstop"), b"x").unwrap();
        assert!(matches!(claim(home.path()), Claim::Unknown));
    }
}
