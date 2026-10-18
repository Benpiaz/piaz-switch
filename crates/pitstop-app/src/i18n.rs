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
            Lang::En => format!("{name} still has room — open Piaz Switch to switch."),
            Lang::Vi => format!("{name} vẫn còn — mở Piaz Switch để chuyển."),
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
