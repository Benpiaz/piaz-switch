//! The tray menu's words, in English and Vietnamese. The window's own words
//! live in `ui/i18n.js`; the two say the same things the same way.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Vi,
}

impl Lang {
    pub fn from_code(code: &str) -> Lang {
        if code.to_ascii_lowercase().starts_with("vi") {
            Lang::Vi
        } else {
            Lang::En
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Vi => "vi",
        }
    }

    /// The language chosen in Settings, else the system's.
    pub fn resolve(chosen: Option<&str>) -> Lang {
        chosen
            .map(Lang::from_code)
            .or_else(|| sys_locale::get_locale().map(|l| Lang::from_code(&l)))
            .unwrap_or(Lang::En)
    }

    fn pick(self, en: &'static str, vi: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Vi => vi,
        }
    }

    pub fn add_account(self) -> &'static str {
        self.pick("Add Account…", "Thêm tài khoản…")
    }
    pub fn refresh(self) -> &'static str {
        self.pick("Refresh", "Làm mới")
    }
    pub fn reading(self) -> &'static str {
        self.pick("Reading…", "Đang đọc…")
    }
    pub fn not_read_yet(self) -> &'static str {
        self.pick("Not read yet", "Chưa đọc")
    }
    pub fn open_pitstop(self) -> &'static str {
        self.pick("Open Piaz Switch", "Mở Piaz Switch")
    }
    pub fn quit(self) -> &'static str {
        self.pick("Quit Piaz Switch", "Thoát Piaz Switch")
    }
    pub fn made_by(self, author: &str) -> String {
        match self {
            Lang::En => format!("Made by {author}"),
            Lang::Vi => format!("Làm bởi {author}"),
        }
    }
    /// A tray item for one of `links::LINKS`.
    pub fn link(self, name: &str) -> &'static str {
        match name {
            "website" => self.pick("Piaz Switch Website", "Trang web Piaz Switch"),
            "repo" => self.pick("Source on GitHub", "Mã nguồn trên GitHub"),
            "youtube" => "YouTube @benpiaz",
            "x" => "X @benpiazai",
            _ => "",
        }
    }
    pub fn couldnt_open(self) -> &'static str {
        self.pick("Couldn’t open the link", "Không mở được link")
    }
    pub fn no_accounts(self) -> &'static str {
        self.pick("No accounts yet", "Chưa có tài khoản")
    }
    pub fn not_named_yet(self) -> &'static str {
        self.pick("Not named yet", "Chưa đặt tên")
    }
    pub fn sign_in_again_in_tool(self) -> &'static str {
        self.pick(
            "Its record names another account: run /login",
            "Hồ sơ ghi tài khoản khác: chạy /login",
        )
    }
    pub fn unknown_login(self) -> &'static str {
        self.pick(
            "Login Piaz Switch can't identify",
            "Login Piaz Switch không nhận ra",
        )
    }
    pub fn needs_sign_in(self) -> &'static str {
        self.pick("Needs signing in again", "Cần đăng nhập lại")
    }
    pub fn token_expired(self) -> &'static str {
        self.pick(
            "Token expired — switch to it to refresh",
            "Token hết hạn — chuyển sang để làm mới",
        )
    }
    pub fn usage_unavailable(self) -> &'static str {
        self.pick("Usage unavailable", "Không đọc được usage")
    }

    pub fn updated_at(self, time: &str) -> String {
        match self {
            Lang::En => format!("Updated {time}"),
            Lang::Vi => format!("Cập nhật lúc {time}"),
        }
    }

    pub fn switch_to(self, name: &str) -> String {
        match self {
            Lang::En => format!("Switch to {name}"),
            Lang::Vi => format!("Chuyển sang {name}"),
        }
    }

    pub fn ran_out(self, name: &str, window: &str) -> String {
        match self {
            Lang::En => format!("{name} has no {window} limit left"),
            Lang::Vi => format!("{name} đã hết giới hạn {window}"),
        }
    }

    /// The line under a run-out notification.
    pub fn still_has_room(self, name: &str) -> String {
        match self {
            Lang::En => format!("{name} still has room."),
            Lang::Vi => format!("{name} vẫn còn chỗ."),
        }
    }

    pub fn auto_switched(self, tool: &str, name: &str) -> String {
        match self {
            Lang::En => format!("Switched {tool} to {name} automatically"),
            Lang::Vi => format!("Đã tự chuyển {tool} sang {name}"),
        }
    }

    pub fn auto_switched_why(self, from: &str, percent: f64, window: &str) -> String {
        match self {
            Lang::En => format!("{from} had used {percent:.0}% of its {window} limit."),
            Lang::Vi => format!("{from} đã dùng {percent:.0}% giới hạn {window}."),
        }
    }

    /// Why a switch came before any limit was reached: `to`'s week resets
    /// first, so what it has left would be lost sooner.
    pub fn auto_switched_early_why(
        self,
        from: &str,
        to: &str,
        to_resets: Option<&str>,
        from_resets: Option<&str>,
    ) -> String {
        match (self, to_resets, from_resets) {
            (Lang::En, Some(to_at), Some(from_at)) => format!(
                "{to}'s week resets at {to_at}, before {from}'s at {from_at}: its quota is spent first."
            ),
            (Lang::En, Some(to_at), None) => format!(
                "{to}'s week resets at {to_at}, before {from}'s: its quota is spent first."
            ),
            (Lang::En, None, _) => format!(
                "{to} has no weekly limit, so what it leaves is lost every 5 hours: it is used first."
            ),
            (Lang::Vi, Some(to_at), Some(from_at)) => format!(
                "Tuần của {to} reset lúc {to_at}, trước {from} ({from_at}): dùng hết quota của {to} trước."
            ),
            (Lang::Vi, Some(to_at), None) => format!(
                "Tuần của {to} reset lúc {to_at}, trước {from}: dùng hết quota của {to} trước."
            ),
            (Lang::Vi, None, _) => format!(
                "{to} không có giới hạn tuần, phần không dùng mất sau mỗi 5 giờ: dùng {to} trước."
            ),
        }
    }

    /// A limit of the account in use at the share (`reached`: used up), for
    /// a tool not switched by itself: `Codex: 5-hour limit almost reached`.
    pub fn near_title(self, tool: &str, window: &str, reached: bool) -> String {
        match (self, reached) {
            (Lang::En, false) => format!("{tool}: {window} limit almost reached"),
            (Lang::En, true) => format!("{tool}: {window} limit reached"),
            (Lang::Vi, false) => format!("{tool}: sắp hết giới hạn {window}"),
            (Lang::Vi, true) => format!("{tool}: đã hết giới hạn {window}"),
        }
    }

    /// Who used how much, and where to go: an account with room, or when
    /// the limit resets when none has any.
    pub fn near_body(
        self,
        from: &str,
        percent: f64,
        window: &str,
        to: Option<&str>,
        resets_in: Option<&str>,
    ) -> String {
        let percent = percent.floor().min(100.0);
        let used = match self {
            Lang::En => format!("{from} has used {percent:.0}% of its {window} limit."),
            Lang::Vi => format!("{from} đã dùng {percent:.0}% giới hạn {window}."),
        };
        let next = match (self, to, resets_in) {
            (Lang::En, Some(to), _) => format!("{to} has room."),
            (Lang::En, None, Some(after)) => {
                format!("No other account has room; it resets in {after}.")
            }
            (Lang::En, None, None) => "No other account has room.".into(),
            (Lang::Vi, Some(to), _) => format!("{to} vẫn còn chỗ."),
            (Lang::Vi, None, Some(after)) => {
                format!("Không tài khoản nào khác còn chỗ; giới hạn reset sau {after}.")
            }
            (Lang::Vi, None, None) => "Không tài khoản nào khác còn chỗ.".into(),
        };
        format!("{used} {next}")
    }

    pub fn switch_back(self) -> &'static str {
        self.pick("Switch back", "Chuyển lại")
    }

    pub fn resets_first_title(self) -> &'static str {
        self.pick(
            "Another account resets first",
            "Có tài khoản reset tuần sớm hơn",
        )
    }

    /// Why `to` is worth using first: its week resets sooner, or it has
    /// none and loses what it leaves every five hours.
    pub fn resets_first_body(
        self,
        from: &str,
        to: &str,
        to_resets: Option<&str>,
        from_resets: Option<&str>,
    ) -> String {
        match (self, to_resets, from_resets) {
            (Lang::En, Some(to_at), Some(from_at)) => format!(
                "{to}'s week resets at {to_at}, before {from}'s at {from_at}. Use it first so its quota isn't lost."
            ),
            (Lang::En, Some(to_at), None) => format!(
                "{to}'s week resets at {to_at}, before {from}'s. Use it first so its quota isn't lost."
            ),
            (Lang::En, None, _) => format!(
                "{to} has no weekly limit, so what it leaves is lost every 5 hours. Use it first."
            ),
            (Lang::Vi, Some(to_at), Some(from_at)) => format!(
                "Tuần của {to} reset lúc {to_at}, trước {from} ({from_at}). Dùng {to} trước để không mất quota."
            ),
            (Lang::Vi, Some(to_at), None) => format!(
                "Tuần của {to} reset lúc {to_at}, trước {from}. Dùng {to} trước để không mất quota."
            ),
            (Lang::Vi, None, _) => format!(
                "{to} không có giới hạn tuần, phần không dùng mất sau mỗi 5 giờ. Dùng {to} trước."
            ),
        }
    }

    pub fn sign_in_lost_title(self) -> &'static str {
        self.pick("Sign-in no longer works", "Đăng nhập không còn dùng được")
    }

    pub fn sign_in_lost_body(self, tool: &str, name: &str) -> String {
        match self {
            Lang::En => format!(
                "{tool} refused {name}'s saved sign-in. Sign in to it again to keep switching to it."
            ),
            Lang::Vi => format!(
                "{tool} từ chối đăng nhập đã lưu của {name}. Đăng nhập lại để tiếp tục chuyển sang tài khoản này."
            ),
        }
    }

    pub fn sign_in_again(self) -> &'static str {
        self.pick("Sign in again", "Đăng nhập lại")
    }

    pub fn cant_auto_switch(self) -> &'static str {
        self.pick(
            "Couldn't switch Claude Code automatically",
            "Không tự chuyển được Claude Code",
        )
    }

    pub fn cant_auto_switch_why(self) -> &'static str {
        self.pick(
            "It is signed in to a login Piaz Switch doesn't manage. Name it first.",
            "Claude Code đang dùng một login chưa đặt tên trong Piaz Switch. Hãy đặt tên trước.",
        )
    }

    /// The tray item when a newer version is out.
    pub fn update_to(self, version: &str) -> String {
        match self {
            Lang::En => format!("Update to Piaz Switch {version}…"),
            Lang::Vi => format!("Cập nhật lên Piaz Switch {version}…"),
        }
    }

    pub fn update_out(self, version: &str) -> String {
        match self {
            Lang::En => format!("Piaz Switch {version} is out"),
            Lang::Vi => format!("Đã có Piaz Switch {version}"),
        }
    }

    pub fn update_out_why(self) -> &'static str {
        self.pick(
            "Open Piaz Switch to get it. Your accounts stay as they are.",
            "Mở Piaz Switch để cập nhật. Các tài khoản vẫn giữ nguyên.",
        )
    }

    /// `5h` → `5-hour`, `week` → `weekly`, `week Opus` → `weekly Opus`,
    /// `month` → `monthly`.
    pub fn window(self, label: &str) -> String {
        let (head, rest) = label.split_once(' ').unwrap_or((label, ""));
        let head = match (self, head) {
            (Lang::En, "5h") => "5-hour",
            (Lang::En, "week") => "weekly",
            (Lang::Vi, "5h") => "5 giờ",
            (Lang::Vi, "week") => "tuần",
            (Lang::En, "month") => "monthly",
            (Lang::Vi, "month") => "tháng",
            (_, other) => other,
        };
        if rest.is_empty() {
            head.to_owned()
        } else {
            format!("{head} {rest}")
        }
    }

    pub fn window_used(self, window: &str, percent: f64) -> String {
        format!("{window} {percent:.0}%")
    }

    pub fn window_used_up(self, window: &str, resets_in: Option<&str>) -> String {
        match (self, resets_in) {
            (Lang::En, Some(after)) => format!("{window} used up, resets in {after}"),
            (Lang::En, None) => format!("{window} used up"),
            (Lang::Vi, Some(after)) => format!("hết {window}, reset sau {after}"),
            (Lang::Vi, None) => format!("hết {window}"),
        }
    }

    // ---------- tray tooltip, Jump List ----------

    pub fn needs_attention(self) -> &'static str {
        self.pick("Needs attention", "Cần xem lại")
    }

    /// An account's limits in the tooltip: `5-hour 42% used, weekly 12%`;
    /// none when it has none.
    pub fn tip_usage(self, parts: &[(String, f64)]) -> Option<String> {
        let (first, rest) = parts.split_first()?;
        let first = match self {
            Lang::En => format!("{} {:.0}% used", first.0, first.1),
            Lang::Vi => format!("đã dùng {} {:.0}%", first.0, first.1),
        };
        Some(
            std::iter::once(first)
                .chain(rest.iter().map(|(w, p)| format!("{w} {p:.0}%")))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// A Jump List task naming the tool too, once several tools have accounts.
    pub fn switch_to_on(self, name: &str, tool: &str) -> String {
        match self {
            Lang::En => format!("Switch to {name} ({tool})"),
            Lang::Vi => format!("Chuyển sang {name} ({tool})"),
        }
    }

    /// The tray's line for an account's site window: `Open claude.ai as work`.
    pub fn open_site_as(self, site: &str, name: &str) -> String {
        match self {
            Lang::En => format!("Open {site} as {name}"),
            Lang::Vi => format!("Mở {site} bằng {name}"),
        }
    }

    pub fn add_account_task(self) -> &'static str {
        self.pick("Add Account", "Thêm tài khoản")
    }

    pub fn couldnt_switch(self, name: &str) -> String {
        match self {
            Lang::En => format!("Couldn't switch to {name}"),
            Lang::Vi => format!("Không chuyển được sang {name}"),
        }
    }

    /// Why a switch from the Jump List didn't happen, in words: `code` is the
    /// error's, `message` its English words for anything not worded here.
    pub fn switch_failed_why(self, code: &str, name: &str, tool: &str, message: &str) -> String {
        match (self, code) {
            (Lang::En, "warmingUp") => {
                format!(
                    "{name} is warming up. Switch to it when the warm-up ends, in a few seconds."
                )
            }
            (Lang::Vi, "warmingUp") => {
                format!("{name} đang warm-up. Chờ vài giây cho xong rồi chuyển.")
            }
            (Lang::En, "editorMissing") => {
                format!("Piaz Switch didn't find {tool} on this computer.")
            }
            (Lang::Vi, "editorMissing") => format!("Piaz Switch không thấy {tool} trên máy này."),
            (Lang::En, "editorStillOpen" | "editorCloseYourself") => {
                format!("{tool} is open. Close it, then switch again.")
            }
            (Lang::Vi, "editorStillOpen" | "editorCloseYourself") => {
                format!("{tool} đang mở. Đóng nó rồi chuyển lại.")
            }
            (Lang::En, "signInRunning") => {
                "Another sign-in is running. Switch when it ends.".to_owned()
            }
            (Lang::Vi, "signInRunning") => {
                "Đang có một lần đăng nhập khác. Chờ xong rồi chuyển.".to_owned()
            }
            (Lang::En, "vaultBlobMissing" | "vaultCorrupt") => {
                format!("Sign in to {name} again in Piaz Switch to switch to it.")
            }
            (Lang::Vi, "vaultBlobMissing" | "vaultCorrupt") => {
                format!("Đăng nhập lại {name} trong Piaz Switch để chuyển sang nó.")
            }
            (Lang::En, "unverified") => format!(
                "Piaz Switch can't tell whose login {tool} is using right now. Try again in a moment."
            ),
            (Lang::Vi, "unverified") => format!(
                "Piaz Switch chưa xác định được {tool} đang dùng login của ai. Thử lại sau giây lát."
            ),
            (Lang::En, "misrecorded") => {
                format!("{tool}'s record names another account than its login. Run /login in {tool}.")
            }
            (Lang::Vi, "misrecorded") => {
                format!("Hồ sơ của {tool} ghi một tài khoản khác với login. Chạy /login trong {tool}.")
            }
            (Lang::En, "unknownAccount") => format!("{name} isn't in Piaz Switch any more."),
            (Lang::Vi, "unknownAccount") => format!("{name} không còn trong Piaz Switch."),
            _ => message.to_owned(),
        }
    }
}

/// `2h 09m`, `4d 2h`, `12m`: short enough for a menu line.
pub fn duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{}m", m.max(1)),
        (0, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}d {h}h"),
    }
}
