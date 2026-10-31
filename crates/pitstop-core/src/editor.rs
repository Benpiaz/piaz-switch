//! The editors among the tools (Cursor, Antigravity): desktop apps, not
//! command lines. Each keeps its login in its own `state.vscdb`, reads it once
//! when it starts and keeps it in memory from then on, so a login is only
//! changed while the editor is closed, and the editor opened again after.
//!
//! Antigravity here is Google's editor built on VS Code: Antigravity IDE
//! since Antigravity 2.0, Antigravity itself before. The Antigravity 2 app is
//! another product: it keeps its login in the OS keyring (Windows Credential
//! Manager, the macOS Keychain), installs where the editor used to be (on
//! Windows into the very folder, where it then runs in the editor's place:
//! see [`editor_product`]), and is never opened, closed or signed in to by
//! Pitstop. Where it goes by the same program name as the editor, that name
//! can't tell the two apart, so Pitstop closes neither (see [`Build`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::provider::ProviderId;

/// One way an editor app is installed.
struct Layout {
    /// Its folder on Windows, under `%LOCALAPPDATA%\Programs` or Program
    /// Files. Windows ignores case there, so `Antigravity` is also the
    /// Antigravity 2 app's `antigravity`.
    windows_folder: &'static str,
    /// Its program in that folder.
    windows_exe: &'static str,
    /// Its bundle on macOS, `<name>.app`; None for a layout only Windows has.
    mac_app: Option<&'static str>,
    /// Its folder under the per-user app data (`%APPDATA%` and the like),
    /// unless its `product.json` names another.
    data_folder: &'static str,
}

impl Layout {
    /// Where it is under an install root: its program on Windows, its bundle
    /// on macOS.
    fn place(&self, os: Os) -> Option<PathBuf> {
        match os {
            Os::Windows => Some(Path::new(self.windows_folder).join(self.windows_exe)),
            Os::Mac => self.mac_app.map(PathBuf::from),
        }
    }

    /// What its process goes by (see [`Build::process`]).
    fn process(&self, os: Os) -> Option<&'static str> {
        match os {
            Os::Windows => Some(self.windows_exe),
            Os::Mac => self.mac_app,
        }
    }
}

/// What Pitstop needs to know of one editor app.
pub struct App {
    pub provider: ProviderId,
    /// What Pitstop looks for, as its makers call it, for saying it wasn't
    /// found: for Antigravity that is its editor, Antigravity IDE.
    pub name: &'static str,
    /// The ways it is installed, newest first: the first found is the one in
    /// use.
    layouts: &'static [Layout],
    /// Whether a build counts only as the editor built on VS Code, known by
    /// the `product.json` it runs (see [`editor_product`]), which also names
    /// its data folder. The Antigravity 2 app, installed where Antigravity's
    /// editor used to be, runs an `app.asar` instead.
    needs_product: bool,
}

const CURSOR: App = App {
    provider: ProviderId::Cursor,
    name: "Cursor",
    layouts: &[Layout {
        windows_folder: "cursor",
        windows_exe: "Cursor.exe",
        mac_app: Some("Cursor.app"),
        data_folder: "Cursor",
    }],
    needs_product: false,
};

const ANTIGRAVITY: App = App {
    provider: ProviderId::Antigravity,
    name: "Antigravity IDE",
    layouts: &[
        // Antigravity IDE, since Antigravity 2.0.
        Layout {
            windows_folder: "Antigravity IDE",
            windows_exe: "Antigravity IDE.exe",
            mac_app: Some("Antigravity IDE.app"),
            data_folder: "Antigravity IDE",
        },
        // The same, in the folder of the editor before 2.0, which the
        // Antigravity 2 installer may have put its app into as well.
        Layout {
            windows_folder: "Antigravity",
            windows_exe: "Antigravity IDE.exe",
            mac_app: None,
            data_folder: "Antigravity IDE",
        },
        // The editor before 2.0, where the Antigravity 2 app installs now.
        Layout {
            windows_folder: "Antigravity",
            windows_exe: "Antigravity.exe",
            mac_app: Some("Antigravity.app"),
            data_folder: "Antigravity",
        },
    ],
    needs_product: true,
};

/// The editor app `provider` is, or None for a command-line tool.
pub fn app(provider: ProviderId) -> Option<&'static App> {
    match provider {
        ProviderId::Cursor => Some(&CURSOR),
        ProviderId::Antigravity => Some(&ANTIGRAVITY),
        ProviderId::ClaudeCode | ProviderId::Codex => None,
    }
}

/// How an app is installed: a program in a folder on Windows, a bundle on
/// macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Mac,
}

impl Os {
    fn this() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else {
            Os::Mac
        }
    }
}

/// One build of an editor app, found installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// Its program on Windows, its bundle on macOS.
    pub path: PathBuf,
    /// Its folder under the per-user app data.
    pub data_folder: String,
    /// Whether a program installed elsewhere that is not this app's goes by
    /// the same process name: the editor before Antigravity 2.0 in Program
    /// Files and the Antigravity 2 app in `%LOCALAPPDATA%\Programs` both run
    /// as `Antigravity.exe`. Closing that name could close the other app, so
    /// such a build is never closed by Pitstop; it still counts as running
    /// while the name runs, so its login is never written under it.
    shared_name: bool,
}

impl Build {
    /// What its process goes by: its program's name on Windows
    /// (`Antigravity IDE.exe`), its bundle's on macOS (`Antigravity IDE.app`).
    fn process(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Whether Pitstop may close it itself: not while another app goes by
    /// its process name. Then the user closes it.
    pub fn closable(&self) -> bool {
        !self.shared_name
    }
}

impl App {
    /// Its `state.vscdb` under a user data directory.
    pub fn state_db_in(user_data: &Path) -> PathBuf {
        user_data
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    }

    /// The `state.vscdb` it uses day to day.
    pub fn state_db(&self, host: &dyn Host) -> PathBuf {
        Self::state_db_in(&host.editor_data(self))
    }

    /// The data folder of its newest build, for a machine with none
    /// installed.
    pub fn default_data_folder(&self) -> &'static str {
        self.layouts[0].data_folder
    }

    /// The folder under the per-user app data the build in use keeps its data
    /// in. `home` is the real home directory, where installers put programs.
    pub fn data_folder(&self, home: &Path) -> String {
        self.find(home).map_or_else(
            || self.default_data_folder().to_owned(),
            |build| build.data_folder,
        )
    }

    /// The build in use, if one is installed: its program on Windows, its
    /// bundle on macOS. `home` is the real home directory.
    pub fn find(&self, home: &Path) -> Option<Build> {
        self.find_under(Os::this(), &install_roots(home))
    }

    /// The first build under `roots`, the newest layout first.
    fn find_under(&self, os: Os, roots: &[PathBuf]) -> Option<Build> {
        let mut build = self.layouts.iter().find_map(|layout| {
            let place = layout.place(os)?;
            roots
                .iter()
                .find_map(|root| self.build_at(os, layout, root.join(&place)))
        })?;
        build.shared_name = self.name_shared(os, roots, &build);
        Some(build)
    }

    /// The build at `path`, if it is there and is one of this app's.
    fn build_at(&self, os: Os, layout: &Layout, path: PathBuf) -> Option<Build> {
        if !path.exists() {
            return None;
        }
        let mut data_folder = layout.data_folder.to_owned();
        if self.needs_product {
            let product = editor_product(os, &path)?;
            if let Some(named) = std::fs::read(&product)
                .ok()
                .and_then(|bytes| named_data_folder(&bytes))
            {
                data_folder = named;
            }
        }
        Some(Build {
            path,
            data_folder,
            shared_name: false,
        })
    }

    /// Whether a program under `roots` that is there but is not one of this
    /// app's builds goes by `build`'s process name (see [`Build`]).
    fn name_shared(&self, os: Os, roots: &[PathBuf], build: &Build) -> bool {
        let name = build.process();
        self.layouts
            .iter()
            .filter(|layout| {
                layout
                    .process(os)
                    .is_some_and(|process| process.eq_ignore_ascii_case(&name))
            })
            .any(|layout| {
                let Some(place) = layout.place(os) else {
                    return false;
                };
                roots.iter().any(|root| {
                    let path = root.join(&place);
                    path != build.path && path.exists() && self.build_at(os, layout, path).is_none()
                })
            })
    }

    /// What its processes may go by: the build in use's, or with none
    /// installed any build's, so a check never misses one.
    fn processes(&self, os: Os, build: Option<&Build>) -> Vec<String> {
        if let Some(build) = build {
            return vec![build.process()];
        }
        let mut names: Vec<String> = Vec::new();
        for name in self.layouts.iter().filter_map(|layout| layout.process(os)) {
            if !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
        names
    }

    /// The build in use, looked for under the real home.
    fn installed(&self) -> Option<Build> {
        self.find(&dirs::home_dir().unwrap_or_default())
    }

    /// Whether it is running now: the build in use, or with none installed
    /// any build it may be. A check that can't be made says it is, so its
    /// login is never written under it.
    pub fn running(&self) -> bool {
        self.running_as(self.installed().as_ref())
    }

    fn running_as(&self, build: Option<&Build>) -> bool {
        self.processes(Os::this(), build)
            .iter()
            .any(|name| process_running(name))
    }

    /// Ask the build in use to close (its windows on Windows, Quit on macOS)
    /// and wait for it to be gone. Whether unsaved files are kept or asked
    /// about is its own setting; one that asks, or doesn't go within `wait`,
    /// is left open and this fails. With none installed, or one whose
    /// process name another app shares, nothing is closed: what runs may be
    /// that other app, and the user closes it.
    pub fn quit(&self, wait: Duration) -> Result<()> {
        let build = self.installed();
        if !self.running_as(build.as_ref()) {
            return Ok(());
        }
        let still_open = || Error::EditorStillOpen(self.provider.display_name().into());
        let Some(build) = build.filter(Build::closable) else {
            return Err(still_open());
        };
        let name = build.process();
        #[cfg(windows)]
        let _ = quiet(Command::new("taskkill").args(["/IM", &name])).status();
        #[cfg(not(windows))]
        let _ = quiet(Command::new("osascript").args([
            "-e",
            &format!("quit app \"{}\"", name.trim_end_matches(".app")),
        ]))
        .status();
        let started = Instant::now();
        while started.elapsed() < wait {
            std::thread::sleep(Duration::from_millis(400));
            if !self.running_as(Some(&build)) {
                // Let it finish letting go of its files.
                std::thread::sleep(Duration::from_millis(600));
                return Ok(());
            }
        }
        Err(still_open())
    }

    /// Open it, on its own: it outlives Pitstop.
    pub fn open(&self, home: &Path) -> Result<()> {
        let found = self
            .find(home)
            .ok_or_else(|| Error::ToolNotFound(self.provider.display_name().into()))?
            .path;
        let mut cmd = if cfg!(windows) {
            Command::new(&found)
        } else {
            let mut open = Command::new("open");
            open.arg("-a").arg(&found);
            open
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(DETACHED_PROCESS);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| Error::SignIn(format!("could not open {}: {e}", found.display())))
    }

    /// A command that runs it with its own data in `user_data`, apart from
    /// the copy in daily use, for a private sign-in.
    /// Its home is the private directory too, so what it keeps under the
    /// home (Antigravity's `~/.gemini`) stays apart from the user's.
    pub fn private_command(&self, home: &Path, user_data: &Path) -> Result<Command> {
        let found = self
            .find(home)
            .ok_or_else(|| Error::ToolNotFound(self.provider.display_name().into()))?
            .path;
        let program = match Os::this() {
            Os::Windows => found,
            Os::Mac => {
                let plist = std::fs::read_to_string(found.join("Contents").join("Info.plist")).ok();
                mac_program(&found, plist.as_deref())
            }
        };
        let mut cmd = Command::new(program);
        cmd.arg("--user-data-dir")
            .arg(user_data)
            .arg("--extensions-dir")
            .arg(user_data.join("extensions"))
            .arg("--new-window")
            .env("HOME", user_data.join("home"))
            .env("USERPROFILE", user_data.join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(cmd)
    }
}

/// Where installers put the editors: `%LOCALAPPDATA%\Programs` (or the same
/// under `home`) and Program Files on Windows, `/Applications` and
/// `~/Applications` on macOS. `home` is the real home directory.
fn install_roots(home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut roots: Vec<PathBuf> = std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Programs"))
            .into_iter()
            .collect();
        roots.push(home.join("AppData").join("Local").join("Programs"));
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            roots.extend(std::env::var_os(var).map(PathBuf::from));
        }
        roots
    } else {
        vec![PathBuf::from("/Applications"), home.join("Applications")]
    }
}

/// The `product.json` of the editor built on VS Code that the program at
/// `path` (a bundle on macOS) runs, if it runs one. Such an editor keeps it
/// in `resources/app`: beside its program on Windows, or in a folder per
/// version beside it (`<version>\resources\app`, as newer VS Code builds
/// have; the one written last), and in its bundle on macOS.
///
/// Electron runs `resources/app.asar` before `resources/app`. The Antigravity
/// 2 installer can put its app's `app.asar` and `Antigravity.exe` into the
/// folder of the editor before 2.0, beside that editor's `app` folder and
/// `Antigravity IDE.exe`; from then on both programs there run the
/// Antigravity 2 app. So a program with an `app.asar` to load runs no editor,
/// whatever `product.json` sits beside it.
fn editor_product(os: Os, path: &Path) -> Option<PathBuf> {
    match os {
        Os::Mac => product_in(&path.join("Contents").join("Resources")),
        Os::Windows => {
            let dir = path.parent()?;
            let resources = dir.join("resources");
            if resources.join("app.asar").exists() {
                return None;
            }
            let flat = resources.join("app").join("product.json");
            if flat.is_file() {
                return Some(flat);
            }
            let newest = newest_version(dir)?;
            // <version>\resources\app\product.json
            let resources = newest.parent()?.parent()?;
            product_in(resources)
        }
    }
}

/// `resources/app/product.json` under `resources`, if it is there and no
/// `app.asar` would run before it.
fn product_in(resources: &Path) -> Option<PathBuf> {
    if resources.join("app.asar").exists() {
        return None;
    }
    let product = resources.join("app").join("product.json");
    product.is_file().then_some(product)
}

/// Of the folders per version in `dir`, `<version>\resources\app\
/// product.json` written last: during an update the next version sits beside
/// the one in use until it takes over.
fn newest_version(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let product = entry
                .path()
                .join("resources")
                .join("app")
                .join("product.json");
            let meta = std::fs::metadata(&product).ok().filter(|m| m.is_file())?;
            Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), product))
        })
        .max()
        .map(|(_, product)| product)
}

/// The data folder a `product.json` names: its `nameShort`, the folder VS
/// Code and its kin keep their data in, when that is one plain folder name.
fn named_data_folder(product: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Product {
        #[serde(rename = "nameShort")]
        name_short: String,
    }
    let product = product.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(product);
    let product: Product = serde_json::from_slice(product).ok()?;
    plain_name(&product.name_short).then_some(product.name_short)
}

/// Whether `name` is one plain file or folder name: nothing that could reach
/// outside the folder it is joined to.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name != "."
        && !name.contains("..")
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

/// The program a macOS bundle runs: the one its `Info.plist` names, else one
/// named as the bundle is (`Cursor.app/Contents/MacOS/Cursor`).
fn mac_program(bundle: &Path, plist: Option<&str>) -> PathBuf {
    let name = plist.and_then(bundle_executable).unwrap_or_else(|| {
        bundle
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    bundle.join("Contents").join("MacOS").join(name)
}

/// The program an XML `Info.plist` names (`CFBundleExecutable`): the
/// `<string>` right after its key. VS Code and its kin often call it
/// `Electron`.
fn bundle_executable(plist: &str) -> Option<String> {
    let (_, after) = plist.split_once("<key>CFBundleExecutable</key>")?;
    let value = after.trim_start().strip_prefix("<string>")?;
    let (name, _) = value.split_once("</string>")?;
    let name = name.trim();
    // A plain file name, nothing escaped.
    (plain_name(name) && !name.contains(['&', '<'])).then(|| name.to_owned())
}

/// Whether a process that goes by `name` runs (see [`Build::process`]). A
/// check that can't be made says one does.
fn process_running(name: &str) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("IMAGENAME eq {name}");
        match quiet(Command::new("tasklist").args(["/FI", &filter, "/FO", "CSV", "/NH"])).output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .to_lowercase()
                .contains(&name.to_lowercase()),
            _ => true,
        }
    }
    #[cfg(not(windows))]
    {
        // The main process runs from the bundle's MacOS folder; its
        // helpers run from Frameworks. pgrep: 0 found, 1 none, else failed.
        let pattern = format!("/{name}/Contents/MacOS/");
        match quiet(Command::new("pgrep").args(["-f", &pattern]))
            .stdout(Stdio::null())
            .status()
        {
            Ok(s) => s.code() != Some(1),
            Err(_) => true,
        }
    }
}

/// A helper command that shows no console window.
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null()).stderr(Stdio::null())
}

/// Open `url` in the default browser.
pub fn open_in_browser(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(Error::SignIn("refusing to open a non-https address".into()));
    }
    #[cfg(windows)]
    let mut cmd = {
        // Not through cmd.exe, which would read the `&`s in the address.
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    quiet(&mut cmd)
        .stdout(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| Error::SignIn(format!("could not open the browser: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    #[test]
    fn only_the_editors_are_apps_and_each_has_its_database() {
        assert!(app(ProviderId::ClaudeCode).is_none());
        assert!(app(ProviderId::Codex).is_none());
        let host = FakeHost::new();
        let cursor = app(ProviderId::Cursor).unwrap();
        assert_eq!(
            cursor.state_db(&host),
            PathBuf::from("/fake-home/app-data/Cursor/User/globalStorage/state.vscdb")
        );
        let antigravity = app(ProviderId::Antigravity).unwrap();
        assert_eq!(
            antigravity.state_db(&host),
            PathBuf::from("/fake-home/app-data/Antigravity IDE/User/globalStorage/state.vscdb")
        );
        assert_eq!(
            App::state_db_in(Path::new("/tmp/x")),
            PathBuf::from("/tmp/x/User/globalStorage/state.vscdb")
        );
    }

    #[test]
    fn only_https_addresses_are_opened() {
        assert!(open_in_browser("file:///etc/passwd").is_err());
        assert!(open_in_browser("javascript:alert(1)").is_err());
    }

    /// `app/product.json` holding `product` in `resources`: what an editor
    /// built on VS Code runs.
    fn editor_code(resources: &Path, product: &str) {
        std::fs::create_dir_all(resources.join("app")).unwrap();
        std::fs::write(resources.join("app").join("product.json"), product).unwrap();
    }

    /// `app.asar` in `resources`: what a plain Electron app, the Antigravity
    /// 2 app among them, runs.
    fn asar(resources: &Path) {
        std::fs::create_dir_all(resources).unwrap();
        std::fs::write(resources.join("app.asar"), "").unwrap();
    }

    /// A Windows program `folder\exe` under `root`, nothing beside it.
    fn windows_program(root: &Path, folder: &str, exe: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(exe), "").unwrap();
        dir.join(exe)
    }

    /// A Windows program `folder\exe` under `root`. With `product`, it is an
    /// editor built on VS Code whose `product.json` holds that; without, a
    /// plain Electron app.
    fn windows_build(root: &Path, folder: &str, exe: &str, product: Option<&str>) -> PathBuf {
        let program = windows_program(root, folder, exe);
        let resources = root.join(folder).join("resources");
        match product {
            Some(product) => editor_code(&resources, product),
            None => asar(&resources),
        }
        program
    }

    /// A macOS bundle `name` under `root`, likewise.
    fn mac_build(root: &Path, name: &str, product: Option<&str>) -> PathBuf {
        let bundle = root.join(name);
        let resources = bundle.join("Contents").join("Resources");
        std::fs::create_dir_all(&resources).unwrap();
        match product {
            Some(product) => editor_code(&resources, product),
            None => asar(&resources),
        }
        bundle
    }

    /// A folder per version `name` in `dir`, as newer VS Code builds keep
    /// beside their program, its `product.json` holding `product` and
    /// written at `written`.
    fn version(dir: &Path, name: &str, product: &str, written: std::time::SystemTime) {
        let resources = dir.join(name).join("resources");
        editor_code(&resources, product);
        std::fs::File::options()
            .write(true)
            .open(resources.join("app").join("product.json"))
            .unwrap()
            .set_modified(written)
            .unwrap();
    }

    fn build(path: PathBuf, data_folder: &str) -> Build {
        Build {
            path,
            data_folder: data_folder.into(),
            shared_name: false,
        }
    }

    #[test]
    fn the_antigravity_2_app_is_never_taken_for_the_editor() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        // Named as the layout is, so it is the check that turns it down
        // where names keep their case too.
        windows_build(root.path(), "Antigravity", "Antigravity.exe", None);
        mac_build(root.path(), "Antigravity.app", None);
        assert_eq!(ANTIGRAVITY.find_under(Os::Windows, &roots), None);
        assert_eq!(ANTIGRAVITY.find_under(Os::Mac, &roots), None);
    }

    #[test]
    fn a_folder_the_antigravity_2_app_was_put_into_runs_no_editor() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        // As the 2.0 installer leaves it: its program and app.asar beside the
        // editor's program and app folder. Electron loads the asar first, so
        // both programs there run the Antigravity 2 app.
        windows_build(
            root.path(),
            "Antigravity",
            "Antigravity IDE.exe",
            Some(r#"{"nameShort":"Antigravity IDE"}"#),
        );
        windows_program(root.path(), "Antigravity", "Antigravity.exe");
        asar(&root.path().join("Antigravity").join("resources"));
        assert_eq!(ANTIGRAVITY.find_under(Os::Windows, &roots), None);

        let bundle = mac_build(root.path(), "Antigravity.app", Some("{}"));
        asar(&bundle.join("Contents").join("Resources"));
        assert_eq!(ANTIGRAVITY.find_under(Os::Mac, &roots), None);
    }

    #[test]
    fn antigravity_ide_comes_before_the_editor_before_2_0() {
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // The older editor sits in the root looked at first.
        let roots = [old.path().to_path_buf(), new.path().to_path_buf()];
        windows_build(
            old.path(),
            "Antigravity",
            "Antigravity.exe",
            Some(r#"{"nameShort":"Antigravity"}"#),
        );
        let ide = windows_build(
            new.path(),
            "Antigravity IDE",
            "Antigravity IDE.exe",
            Some(r#"{"nameShort":"Antigravity IDE"}"#),
        );
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(ide, "Antigravity IDE"))
        );

        mac_build(
            old.path(),
            "Antigravity.app",
            Some(r#"{"nameShort":"Antigravity"}"#),
        );
        let ide = mac_build(
            new.path(),
            "Antigravity IDE.app",
            Some(r#"{"nameShort":"Antigravity IDE"}"#),
        );
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Mac, &roots),
            Some(build(ide, "Antigravity IDE"))
        );
    }

    #[test]
    fn the_editor_before_2_0_keeps_its_own_data_folder() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        let old = windows_build(root.path(), "Antigravity", "Antigravity.exe", Some("{}"));
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(old, "Antigravity"))
        );
        let old = mac_build(root.path(), "Antigravity.app", Some("{}"));
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Mac, &roots),
            Some(build(old, "Antigravity"))
        );
    }

    #[test]
    fn antigravity_ide_installed_in_the_old_folder_is_the_ide() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        // The program the editor before left there stays beside it; with no
        // app.asar in the folder, both run the editor's app folder.
        windows_program(root.path(), "Antigravity", "Antigravity.exe");
        let ide = windows_build(
            root.path(),
            "Antigravity",
            "Antigravity IDE.exe",
            Some("{}"),
        );
        let resources = root.path().join("Antigravity").join("resources");
        assert!(!resources.join("app.asar").exists());
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(ide, "Antigravity IDE"))
        );
    }

    #[test]
    fn an_editor_with_a_folder_per_version_runs_the_newest() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        let ide = windows_program(root.path(), "Antigravity IDE", "Antigravity IDE.exe");
        let dir = root.path().join("Antigravity IDE");
        assert_eq!(ANTIGRAVITY.find_under(Os::Windows, &roots), None);

        let at = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        // Named so that neither the order of the folder nor of the names
        // would pick the newer one by chance.
        version(&dir, "ffffffffff", r#"{"nameShort":"Older"}"#, at);
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(ide.clone(), "Older"))
        );
        version(
            &dir,
            "0000000000",
            r#"{"nameShort":"Newer"}"#,
            at + Duration::from_secs(60),
        );
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(ide.clone(), "Newer"))
        );

        // The newest version runs an app.asar: no editor, the older one
        // isn't what runs either.
        asar(&dir.join("0000000000").join("resources"));
        assert_eq!(ANTIGRAVITY.find_under(Os::Windows, &roots), None);
    }

    #[test]
    fn an_app_asar_beside_the_program_comes_before_any_version() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        windows_program(root.path(), "Antigravity IDE", "Antigravity IDE.exe");
        let dir = root.path().join("Antigravity IDE");
        version(&dir, "2c33b50b16", "{}", std::time::SystemTime::now());
        asar(&dir.join("resources"));
        assert_eq!(ANTIGRAVITY.find_under(Os::Windows, &roots), None);
    }

    #[test]
    fn the_app_folder_beside_the_program_comes_before_the_versions() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        let ide = windows_build(
            root.path(),
            "Antigravity IDE",
            "Antigravity IDE.exe",
            Some(r#"{"nameShort":"Beside"}"#),
        );
        let dir = root.path().join("Antigravity IDE");
        version(
            &dir,
            "6d62b1d5da",
            r#"{"nameShort":"Version"}"#,
            std::time::SystemTime::now(),
        );
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &roots),
            Some(build(ide, "Beside"))
        );
    }

    #[test]
    fn a_build_whose_process_name_another_app_goes_by_is_left_to_the_user_to_close() {
        let (local, program_files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // %LOCALAPPDATA%\Programs is looked at first.
        let roots = [
            local.path().to_path_buf(),
            program_files.path().to_path_buf(),
        ];
        windows_build(local.path(), "Antigravity", "Antigravity.exe", None);
        let old = windows_build(
            program_files.path(),
            "Antigravity",
            "Antigravity.exe",
            Some("{}"),
        );
        let found = ANTIGRAVITY.find_under(Os::Windows, &roots).unwrap();
        assert_eq!(found.path, old);
        assert!(!found.closable());
        // It still counts as running while that name runs.
        assert_eq!(
            ANTIGRAVITY.processes(Os::Windows, Some(&found)),
            ["Antigravity.exe"]
        );

        mac_build(local.path(), "Antigravity.app", None);
        let old = mac_build(program_files.path(), "Antigravity.app", Some("{}"));
        let found = ANTIGRAVITY.find_under(Os::Mac, &roots).unwrap();
        assert_eq!(found.path, old);
        assert!(!found.closable());

        // Antigravity IDE, while the folder the 2 app was put into still has
        // an `Antigravity IDE.exe` running that app.
        let root = tempfile::tempdir().unwrap();
        let ide = windows_build(
            root.path(),
            "Antigravity IDE",
            "Antigravity IDE.exe",
            Some("{}"),
        );
        windows_build(
            root.path(),
            "Antigravity",
            "Antigravity IDE.exe",
            Some("{}"),
        );
        asar(&root.path().join("Antigravity").join("resources"));
        let found = ANTIGRAVITY
            .find_under(Os::Windows, &[root.path().to_path_buf()])
            .unwrap();
        assert_eq!(found.path, ide);
        assert!(!found.closable());

        // Alone, the name is the editor's, and Pitstop may close it.
        let alone = tempfile::tempdir().unwrap();
        windows_build(alone.path(), "Antigravity", "Antigravity.exe", Some("{}"));
        let found = ANTIGRAVITY
            .find_under(Os::Windows, &[alone.path().to_path_buf()])
            .unwrap();
        assert!(found.closable());
    }

    #[test]
    fn the_data_folder_is_the_one_product_json_names_if_it_is_plain() {
        assert_eq!(
            named_data_folder(br#"{"nameShort":"Antigravity Insiders","nameLong":"x"}"#),
            Some("Antigravity Insiders".into())
        );
        assert_eq!(
            named_data_folder(b"\xEF\xBB\xBF{\"nameShort\":\"Antigravity IDE\"}"),
            Some("Antigravity IDE".into())
        );
        for unsafe_or_missing in [
            r#"{"nameShort":""}"#,
            r#"{"nameShort":" "}"#,
            r#"{"nameShort":"."}"#,
            r#"{"nameShort":".."}"#,
            r#"{"nameShort":"..\\Roaming"}"#,
            r#"{"nameShort":"a\\b"}"#,
            r#"{"nameShort":"a/b"}"#,
            r#"{"nameShort":"/etc"}"#,
            r#"{"nameShort":"C:"}"#,
            r#"{"nameShort":"a\nb"}"#,
            r#"{"nameShort":3}"#,
            r#"{"nameLong":"Antigravity"}"#,
            "not json",
            "",
        ] {
            assert_eq!(
                named_data_folder(unsafe_or_missing.as_bytes()),
                None,
                "{unsafe_or_missing}"
            );
        }

        // A build whose product.json names no plain folder keeps its layout's.
        let root = tempfile::tempdir().unwrap();
        let ide = windows_build(
            root.path(),
            "Antigravity IDE",
            "Antigravity IDE.exe",
            Some(r#"{"nameShort":"..\\..\\Elsewhere"}"#),
        );
        assert_eq!(
            ANTIGRAVITY.find_under(Os::Windows, &[root.path().to_path_buf()]),
            Some(build(ide, "Antigravity IDE"))
        );
    }

    #[test]
    fn only_the_build_in_use_is_checked_for_or_with_none_every_build() {
        let ide = build(
            PathBuf::from("Programs/Antigravity IDE/Antigravity IDE.exe"),
            "Antigravity IDE",
        );
        assert_eq!(
            ANTIGRAVITY.processes(Os::Windows, Some(&ide)),
            ["Antigravity IDE.exe"]
        );
        let bundle = build(
            PathBuf::from("/Applications/Antigravity IDE.app"),
            "Antigravity IDE",
        );
        assert_eq!(
            ANTIGRAVITY.processes(Os::Mac, Some(&bundle)),
            ["Antigravity IDE.app"]
        );
        assert_eq!(
            ANTIGRAVITY.processes(Os::Windows, None),
            ["Antigravity IDE.exe", "Antigravity.exe"]
        );
        assert_eq!(
            ANTIGRAVITY.processes(Os::Mac, None),
            ["Antigravity IDE.app", "Antigravity.app"]
        );
        assert_eq!(CURSOR.processes(Os::Windows, None), ["Cursor.exe"]);
        assert_eq!(CURSOR.processes(Os::Mac, None), ["Cursor.app"]);
    }

    #[test]
    fn cursor_is_found_as_before() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        assert_eq!(CURSOR.find_under(Os::Windows, &roots), None);
        // No product.json is asked of it, and none is read.
        let cursor = windows_build(root.path(), "cursor", "Cursor.exe", None);
        assert_eq!(
            CURSOR.find_under(Os::Windows, &roots),
            Some(build(cursor, "Cursor"))
        );
        let bundle = mac_build(root.path(), "Cursor.app", Some(r#"{"nameShort":"Other"}"#));
        assert_eq!(
            CURSOR.find_under(Os::Mac, &roots),
            Some(build(bundle, "Cursor"))
        );
        assert_eq!(CURSOR.default_data_folder(), "Cursor");
        assert_eq!(ANTIGRAVITY.default_data_folder(), "Antigravity IDE");
    }

    #[test]
    fn a_bundle_runs_the_program_its_info_plist_names() {
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>CFBundleDisplayName</key>
	<string>Antigravity IDE</string>
	<key>CFBundleExecutable</key>
	<string>Electron</string>
</dict>
</plist>"#;
        assert_eq!(bundle_executable(plist), Some("Electron".into()));
        assert_eq!(
            bundle_executable("<key>CFBundleExecutable</key><string>Antigravity IDE</string>"),
            Some("Antigravity IDE".into())
        );
        for unreadable in [
            "",
            "<key>CFBundleName</key><string>Electron</string>",
            "<key>CFBundleExecutable</key><string></string>",
            "<key>CFBundleExecutable</key><string>../../bin/sh</string>",
            "<key>CFBundleExecutable</key><string>A &amp; B</string>",
            "<key>CFBundleExecutable</key>\n<integer>1</integer>",
            "<key>CFBundleExecutable</key><string>Electron",
        ] {
            assert_eq!(bundle_executable(unreadable), None, "{unreadable}");
        }

        let bundle = Path::new("/Applications/Antigravity IDE.app");
        assert_eq!(
            mac_program(bundle, Some(plist)),
            bundle.join("Contents").join("MacOS").join("Electron")
        );
        // Unreadable, it runs the program named as the bundle, as before.
        assert_eq!(
            mac_program(bundle, None),
            bundle
                .join("Contents")
                .join("MacOS")
                .join("Antigravity IDE")
        );
        assert_eq!(
            mac_program(bundle, Some("not a plist")),
            bundle
                .join("Contents")
                .join("MacOS")
                .join("Antigravity IDE")
        );
    }
}
