//! How the tray icon looks and what its tooltip says, decided from a
//! snapshot: a bar under the mark that empties as the account in use fills
//! its fullest limit, and a badge when something needs attention. Pure: the
//! drawing works on RGBA bytes and nothing here touches the tray.

// The macOS menu bar shows a title instead: there only `fullest` is used.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use crate::i18n::Lang;
use crate::model::{AccountView, LiveView, Snapshot, UsageView, WindowView};

/// How much of the bar is lit: what the account in use has left of the limit
/// the menu bar title shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    /// Nobody signed in, or no reading.
    #[default]
    Unlit,
    /// Under 70% used.
    Full,
    /// From 70% used.
    Half,
    /// From 90% used.
    Sliver,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    pub level: Level,
    /// A login needs signing in again, an account ran out with another to
    /// switch to, a tool with accounts isn't on the computer, or the
    /// accounts can't be read.
    pub attention: bool,
}

/// The account the title and the bar follow: the first tool's account in use.
fn shown_account(snapshot: &Snapshot) -> Option<&AccountView> {
    snapshot
        .tools
        .iter()
        .find_map(|t| t.accounts.iter().find(|a| a.in_use))
}

/// The limits that speak for an account: those for all models, or every
/// limit when it has only per-model ones. A per-model limit nobody used is
/// left out.
fn general(windows: &[WindowView]) -> Vec<&WindowView> {
    let windows: Vec<&WindowView> = windows.iter().filter(|w| !w.quiet).collect();
    let general: Vec<&WindowView> = windows
        .iter()
        .copied()
        .filter(|w| !w.label.starts_with("week "))
        .collect();
    if general.is_empty() {
        windows
    } else {
        general
    }
}

/// The share used of `account`'s fullest limit, where it has a reading.
pub fn fullest(account: &AccountView) -> Option<f64> {
    match &account.usage {
        Some(UsageView::Ok { windows }) => general(windows)
            .into_iter()
            .map(|w| w.percent)
            .reduce(f64::max),
        _ => None,
    }
}

fn level_of(percent: Option<f64>) -> Level {
    match percent {
        None => Level::Unlit,
        Some(p) if p >= 90.0 => Level::Sliver,
        Some(p) if p >= 70.0 => Level::Half,
        Some(_) => Level::Full,
    }
}

pub fn look(snapshot: &Snapshot) -> Look {
    let level = level_of(shown_account(snapshot).and_then(fullest));
    let refused = snapshot
        .tools
        .iter()
        .flat_map(|t| &t.accounts)
        .any(|a| matches!(a.usage, Some(UsageView::Unauthorized)));
    let missing = snapshot
        .tools
        .iter()
        .any(|t| !t.accounts.is_empty() && t.installed == Some(false));
    let attention =
        snapshot.error.is_some() || refused || missing || !snapshot.suggestions.is_empty();
    Look { level, attention }
}

/// The most a Windows tooltip shows, in UTF-16 units.
const TIP_MOST: usize = 127;

/// What the tray icon's tooltip says: the app, whether something needs
/// attention, and a line for each account in use with how much of each limit
/// it has used, its tool after it once several tools have accounts.
pub fn tooltip(snapshot: &Snapshot, lang: Lang) -> String {
    let mut lines = vec!["Piaz Switch".to_owned()];
    if look(snapshot).attention {
        lines.push(lang.needs_attention().to_owned());
    }
    let several = snapshot
        .tools
        .iter()
        .filter(|t| !t.accounts.is_empty())
        .count()
        > 1;
    for tool in &snapshot.tools {
        let Some(account) = tool.accounts.iter().find(|a| a.in_use) else {
            continue;
        };
        let name = if several {
            format!("{} ({})", account.name, tool.name)
        } else {
            account.name.clone()
        };
        let what = match &account.usage {
            Some(UsageView::Ok { windows }) => {
                let parts: Vec<(String, f64)> = general(windows)
                    .into_iter()
                    .map(|w| (lang.window(&w.label), w.percent))
                    .collect();
                lang.tip_usage(&parts)
            }
            Some(UsageView::Expired) => Some(lang.token_expired().to_owned()),
            Some(UsageView::Unauthorized) => Some(lang.needs_sign_in().to_owned()),
            Some(UsageView::Failed { .. }) => Some(lang.usage_unavailable().to_owned()),
            None => None,
        };
        lines.push(match what {
            Some(what) => format!("{name}: {what}"),
            None => name,
        });
    }
    // Signed in to someone not named yet: say so rather than nothing.
    if lines.len() == 1 + usize::from(look(snapshot).attention)
        && snapshot
            .tools
            .iter()
            .any(|t| matches!(t.live, LiveView::Unmanaged { .. }))
    {
        lines.push(lang.not_named_yet().to_owned());
    }
    cap(&lines.join("\n"), TIP_MOST)
}

/// `text` cut to `most` UTF-16 units, an ellipsis marking the cut.
fn cap(text: &str, most: usize) -> String {
    if text.encode_utf16().count() <= most {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let n = c.len_utf16();
        if used + n > most - 1 {
            break;
        }
        out.push(c);
        used += n;
    }
    out.push('…');
    out
}

// ---------- drawing ----------
//
// Coordinates are on a 32 x 32 grid, scaled to the icon's real size: the bar
// runs along the bottom under the mark, the badge sits at the top right.

const GRID: f32 = 32.0;
/// The bar: left, top, right, bottom.
const BAR: (f32, f32, f32, f32) = (4.0, 27.0, 28.0, 31.0);
/// Cleared around the bar so it reads apart from the mark.
const BAR_GAP: f32 = 1.0;
const BADGE_CENTER: (f32, f32) = (26.0, 6.0);
const BADGE_RADIUS: f32 = 5.5;
/// Cleared around the badge.
const BADGE_GAP: f32 = 1.3;

const TRACK: [u8; 4] = [142, 142, 147, 170];
const GREEN: [u8; 3] = [52, 199, 89];
const AMBER: [u8; 3] = [255, 159, 10];
const RED: [u8; 3] = [255, 59, 48];
const BADGE: [u8; 3] = [255, 184, 0];
const BADGE_INK: [u8; 3] = [29, 29, 31];

/// How far along the bar the lit part reaches, on the grid.
fn lit_to(level: Level) -> f32 {
    let (left, _, right, _) = BAR;
    match level {
        Level::Unlit => left,
        Level::Full => right,
        Level::Half => left + (right - left) / 2.0,
        Level::Sliver => left + 4.0,
    }
}

fn lit_color(level: Level) -> [u8; 3] {
    match level {
        Level::Full | Level::Unlit => GREEN,
        Level::Half => AMBER,
        Level::Sliver => RED,
    }
}

/// `base` (RGBA, `width` x `height`) with the bar and, when needed, the
/// badge drawn on it.
pub fn draw(base: &[u8], width: u32, height: u32, look: Look) -> Vec<u8> {
    let mut out = base.to_vec();
    let scale = width as f32 / GRID;
    let (left, top, right, bottom) = BAR;
    let lit = lit_to(look.level);
    let color = lit_color(look.level);
    for py in 0..height {
        for px in 0..width {
            // The pixel's centre, on the grid.
            let x = (px as f32 + 0.5) / scale;
            let y = (py as f32 + 0.5) / scale;
            let i = ((py * width + px) * 4) as usize;
            let Some(pixel) = out.get_mut(i..i + 4) else {
                continue;
            };
            let in_gap = x >= left - BAR_GAP
                && x < right + BAR_GAP
                && y >= top - BAR_GAP
                && y < bottom + BAR_GAP;
            if in_gap {
                pixel.copy_from_slice(&[0, 0, 0, 0]);
                if x >= left && x < right && y >= top && y < bottom {
                    // Rounded ends: the corner pixels are half there.
                    let corner = (x < left + 1.0 || x >= right - 1.0)
                        && (y < top + 1.0 || y >= bottom - 1.0);
                    let rgba = if x < lit {
                        [color[0], color[1], color[2], 255]
                    } else {
                        TRACK
                    };
                    let alpha = if corner { rgba[3] / 2 } else { rgba[3] };
                    pixel.copy_from_slice(&[rgba[0], rgba[1], rgba[2], alpha]);
                }
            }
            if look.attention {
                let (cx, cy) = BADGE_CENTER;
                let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                if d < BADGE_RADIUS + BADGE_GAP {
                    // Coverage of the disc at this pixel, for a soft edge.
                    let cover = (BADGE_RADIUS + 0.5 - d).clamp(0.0, 1.0);
                    let mark = (25.0..27.0).contains(&x)
                        && ((2.5..7.5).contains(&y) || (8.5..10.5).contains(&y));
                    let rgb = if mark { BADGE_INK } else { BADGE };
                    pixel.copy_from_slice(&[rgb[0], rgb[1], rgb[2], (cover * 255.0) as u8]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Suggestion, ToolView, UiError};

    fn window(label: &str, percent: f64) -> WindowView {
        WindowView {
            label: label.into(),
            percent,
            resets_at: None,
            length_secs: None,
            quiet: false,
            idle: false,
        }
    }

    fn account(name: &str, in_use: bool, usage: Option<UsageView>) -> AccountView {
        AccountView {
            name: name.into(),
            label: None,
            plan: None,
            in_use,
            parked_at: None,
            usage,
        }
    }

    fn ok(windows: Vec<WindowView>) -> Option<UsageView> {
        Some(UsageView::Ok { windows })
    }

    fn tool(id: &'static str, name: &'static str, accounts: Vec<AccountView>) -> ToolView {
        ToolView {
            id,
            name,
            installed: Some(true),
            live: LiveView::LoggedOut,
            accounts,
        }
    }

    fn snapshot(tools: Vec<ToolView>) -> Snapshot {
        Snapshot {
            revision: 1,
            now: 0,
            reading: false,
            updated_at: Some(0),
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

    fn claude_using(percent_5h: f64, percent_week: f64) -> Snapshot {
        snapshot(vec![tool(
            "claude-code",
            "Claude Code",
            vec![
                account(
                    "work",
                    true,
                    ok(vec![window("5h", percent_5h), window("week", percent_week)]),
                ),
                account("personal", false, ok(vec![window("5h", 10.0)])),
            ],
        )])
    }

    #[test]
    fn the_bar_empties_as_the_fullest_limit_fills() {
        assert_eq!(look(&claude_using(42.0, 12.0)).level, Level::Full);
        assert_eq!(look(&claude_using(42.0, 70.0)).level, Level::Half);
        assert_eq!(look(&claude_using(90.0, 12.0)).level, Level::Sliver);
        assert_eq!(look(&claude_using(100.0, 12.0)).level, Level::Sliver);
    }

    #[test]
    fn nobody_in_use_or_no_reading_leaves_the_bar_unlit() {
        let nobody = snapshot(vec![tool(
            "claude-code",
            "Claude Code",
            vec![account("work", false, ok(vec![window("5h", 10.0)]))],
        )]);
        assert_eq!(look(&nobody), Look::default());
        let unread = snapshot(vec![tool(
            "claude-code",
            "Claude Code",
            vec![account("work", true, None)],
        )]);
        assert_eq!(look(&unread).level, Level::Unlit);
    }

    #[test]
    fn a_per_model_limit_counts_only_when_there_is_no_other() {
        let s = snapshot(vec![tool(
            "claude-code",
            "Claude Code",
            vec![account(
                "work",
                true,
                ok(vec![window("5h", 20.0), window("week Opus", 95.0)]),
            )],
        )]);
        assert_eq!(look(&s).level, Level::Full);
        let models = snapshot(vec![tool(
            "antigravity",
            "Antigravity",
            vec![account("g", true, ok(vec![window("week Gemini", 95.0)]))],
        )]);
        assert_eq!(look(&models).level, Level::Sliver);
    }

    #[test]
    fn attention_comes_from_a_refused_login_a_run_out_a_missing_tool_or_an_error() {
        assert!(!look(&claude_using(42.0, 12.0)).attention);

        let mut refused = claude_using(42.0, 12.0);
        refused.tools[0].accounts[1].usage = Some(UsageView::Unauthorized);
        assert!(look(&refused).attention);

        let mut ran_out = claude_using(100.0, 12.0);
        ran_out.suggestions.push(Suggestion {
            provider: "claude-code",
            from: "work".into(),
            to: "personal".into(),
            window: "5h".into(),
        });
        assert!(look(&ran_out).attention);

        let mut missing = claude_using(42.0, 12.0);
        missing.tools[0].installed = Some(false);
        assert!(look(&missing).attention);
        // A tool with no accounts may well not be installed.
        let mut other = claude_using(42.0, 12.0);
        other.tools.push(ToolView {
            installed: Some(false),
            ..tool("codex", "Codex", vec![])
        });
        assert!(!look(&other).attention);

        let mut broken = claude_using(42.0, 12.0);
        broken.error = Some(UiError::new("io", "no"));
        assert!(look(&broken).attention);
    }

    #[test]
    fn the_tooltip_names_each_account_in_use_and_what_it_used() {
        assert_eq!(
            tooltip(&claude_using(42.0, 12.0), Lang::En),
            "Piaz Switch\nwork: 5-hour 42% used, weekly 12%"
        );
        let mut two = claude_using(42.0, 12.0);
        two.tools[0].accounts[1].usage = Some(UsageView::Unauthorized);
        two.tools.push(tool(
            "codex",
            "Codex",
            vec![account("main", true, ok(vec![window("5h", 3.0)]))],
        ));
        assert_eq!(
            tooltip(&two, Lang::En),
            "Piaz Switch\nNeeds attention\nwork (Claude Code): 5-hour 42% used, weekly 12%\nmain (Codex): 5-hour 3% used"
        );
        assert_eq!(
            tooltip(&claude_using(42.0, 12.0), Lang::Vi),
            "Piaz Switch\nwork: đã dùng 5 giờ 42%, tuần 12%"
        );
    }

    #[test]
    fn a_long_tooltip_is_cut_to_what_windows_shows() {
        let accounts = (0..12)
            .map(|i| {
                tool(
                    "claude-code",
                    "Claude Code",
                    vec![account(
                        &format!("account-number-{i}"),
                        true,
                        ok(vec![window("5h", 42.0)]),
                    )],
                )
            })
            .collect();
        let tip = tooltip(&snapshot(accounts), Lang::En);
        assert_eq!(tip.encode_utf16().count(), TIP_MOST);
        assert!(tip.ends_with('…'));
        assert_eq!(cap("ngắn", 127), "ngắn");
    }

    const ORANGE: [u8; 4] = [242, 147, 71, 255];

    fn base(size: u32) -> Vec<u8> {
        ORANGE.repeat((size * size) as usize)
    }

    fn at(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn the_bar_is_lit_as_far_as_the_level_and_the_mark_is_left_alone() {
        let full = draw(
            &base(32),
            32,
            32,
            Look {
                level: Level::Full,
                attention: false,
            },
        );
        assert_eq!(at(&full, 32, 16, 28), [52, 199, 89, 255]);
        assert_eq!(at(&full, 32, 16, 26)[3], 0, "a clear gap above the bar");
        assert_eq!(at(&full, 32, 16, 16), ORANGE);
        assert_eq!(at(&full, 32, 26, 6), ORANGE, "no badge");

        let half = draw(
            &base(32),
            32,
            32,
            Look {
                level: Level::Half,
                attention: false,
            },
        );
        assert_eq!(at(&half, 32, 8, 28), [255, 159, 10, 255]);
        assert_eq!(at(&half, 32, 24, 28), TRACK);

        let sliver = draw(
            &base(32),
            32,
            32,
            Look {
                level: Level::Sliver,
                attention: false,
            },
        );
        assert_eq!(at(&sliver, 32, 5, 28), [255, 59, 48, 255]);
        assert_eq!(at(&sliver, 32, 12, 28), TRACK);

        let unlit = draw(&base(32), 32, 32, Look::default());
        assert_eq!(at(&unlit, 32, 5, 28), TRACK);
    }

    #[test]
    fn the_badge_shows_only_when_something_needs_attention() {
        let badge = draw(
            &base(32),
            32,
            32,
            Look {
                level: Level::Full,
                attention: true,
            },
        );
        assert_eq!(
            at(&badge, 32, 25, 4),
            [29, 29, 31, 255],
            "the mark's stroke"
        );
        assert_eq!(at(&badge, 32, 23, 6), [255, 184, 0, 255], "the badge");
        assert_eq!(at(&badge, 32, 19, 6)[3], 0, "cleared around it");
        assert_eq!(at(&badge, 32, 12, 16), ORANGE);
    }

    #[test]
    fn drawing_follows_the_icon_size() {
        let big = draw(
            &base(64),
            64,
            64,
            Look {
                level: Level::Full,
                attention: false,
            },
        );
        assert_eq!(at(&big, 64, 32, 57), [52, 199, 89, 255]);
        assert_eq!(at(&big, 64, 32, 32), ORANGE);
    }
}
