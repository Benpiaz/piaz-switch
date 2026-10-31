# Changelog

## [Unreleased]

### Fixed
- A parked Claude Code login that can no longer be renewed says **sign in again**,
  with the button beside it, instead of "switching renews it". When Anthropic
  refused a renewal, during a warm-up or in Claude Code, Claude Code blanked the
  login's tokens, and Piaz Switch read what was left as a token that had only
  lapsed: the warm-up said the login couldn't be renewed while the account's row
  promised a switch would renew it, and a switch left Claude Code signed out. A
  login whose refresh token is past the date it came with reads the same way.

## [0.6.0] - 2026-11-04

### Added
- **Use first** in Settings → Automation: **Most left** (as before) or **Week that
  resets first**. Each Claude Code account's week resets at its own time, and what
  it leaves unused is lost then. With the second, the automatic switch, and the
  account offered once one runs out, go first to the account whose week resets
  soonest; with automatic switching on, Piaz Switch also moves Claude Code there
  before the share is reached when it has room and has used no more of its 5 hours,
  at most once a week per account, never within 5 minutes of a switch and never back
  to an account you switched away from yourself that week. An account without a
  weekly limit, such as a Team seat, comes first. The notification says why: "work's
  week resets at Tue 21:00, before personal's at Sun 18:00".
- **Activity**, in Settings → Troubleshoot: the last 500 changes to your accounts,
  newest first, by day: switches by hand and automatic ones (with why), accounts
  added, signed in again, renamed and forgotten, parked logins renewed, warm-ups, and
  what stopped any that failed, whether they came from the app or the command line.
  `piaz-switch log` shows the same. It is kept in `~/.pitstop/activity.jsonl` and
  holds names and outcome codes only, never a token or an email.
- **Diagnostics**, in Settings → Troubleshoot: checks Piaz Switch's folder, the
  account list, the vault key, and for each tool its program, the login in use (and
  whether a sign-in such as `/login` replaced it outside Piaz Switch), every parked
  login and each account's last usage read, with a fix beside what the window can
  mend (**Sign In Again**, **Name It**, **Refresh**). **Copy Report** copies the
  checks for a bug report, with paths starting at `~` and no email addresses.
  `piaz-switch doctor` runs the same checks, except the usage reads, and exits with
  1 when one fails.
- **Notifications you can act on.** **Notify when a limit reaches the share**
  (Settings → Automation, on by default) tells you once per limit and reset when the
  account in use reaches the share, for Codex, Cursor and Antigravity, and for Claude
  Code while it isn't switched automatically. It names the account to switch to (by
  **Use first** for Claude Code) with a **Switch to …** button, or says when the limit
  resets when no other account has room. Under **Week that resets first** without
  automatic switching, the account whose week resets first is offered the same way,
  once per account and week. A login the service refused is notified once, with
  **Sign in again**. The run-out notice gets a **Switch to …** button and the
  automatic switch's notice a **Switch back** button. A click on a notification opens
  Piaz Switch, and what was notified isn't posted again after a restart.
- **A switch you make stays made.** After **Switch back**, or any switch of your own
  (window, tray, Jump List, notification), the automatic switch no longer moves
  Claude Code away from that account for a limit it was already at the share of
  when you chose it, until that limit resets; a limit that reaches the share later
  still switches. With **Week that resets first**, no switch before the share takes
  Claude Code away from an account you chose that week.
- **A tray icon that shows how much is left, a Jump List and a welcome.** On Windows
  the tray icon carries a bar under the mark: full while the account in use has used
  under 70% of its fullest limit, half from 70%, a sliver from 90%, and a badge when
  something needs you (a login to sign in again, an account that ran out, a tool that
  went missing). Its tooltip names each account in use with how much of each limit it
  used. Right-clicking Piaz Switch on the taskbar, or where it's pinned in Start,
  offers **Switch to …** for each account and **Add Account**; a switch from there
  happens without opening the window, and a notification says why when it can't. The
  first time Piaz Switch opens with no account, a welcome sheet shows the tools it
  found, where its icon lives, and **Open at login**.
- **A claude.ai or chatgpt.com window for each account.** An account's menu offers
  **Open claude.ai** (Claude Code) or **Open chatgpt.com** (Codex), and the tray menu
  does the same for the account in use. Each window keeps a web profile of its own,
  so it stays signed in to that account on the site (sign in once there; on macOS
  this needs macOS 14 or later, older versions open the site in your browser). The
  page can't reach Piaz Switch and links elsewhere open in your browser (on macOS,
  where the window can't tell a frame from the page, only new windows do). Renaming
  the account keeps its web sign-in (on macOS when Piaz Switch knows who the account
  is; otherwise the site asks you to sign in again); forgetting it deletes its web
  profile, and one a closing window still held is deleted the next time Piaz Switch
  starts. `piaz-switch rename` and `remove` do the same for the profile folders.
- **Claude Code status line** (Settings → Command line): **Status line in Claude
  Code** adds a line under every Claude Code session with each account's 5-hour and
  weekly use, the one in use first with its pace (▲ ahead, ▼ behind), read from what
  Piaz Switch last read (`?` when that's over 15 minutes old). It's added only where
  Claude Code has no status line of its own, keeps every other setting, and
  **Remove** takes out only ours; `piaz-switch statusline` prints the same.
- **Keep saved sign-ins fresh** (Settings → Command line): Task Scheduler (Windows)
  or a LaunchAgent (macOS) renews parked Claude Code logins once a day while Piaz
  Switch is closed, so one left alone for weeks doesn't expire; on Windows it also
  runs on battery and catches up once the PC wakes. It never runs alongside the open
  app (a status line run in a Claude Code session doesn't count as the app). `piaz-switch renew` and
  `piaz-switch schedule install|uninstall|status` do the same from the command line.

## [0.5.5] - 2026-11-03

### Fixed
- A parked Claude Code account no longer shows "Token expired" with no limits
  for hours. A login is parked with whatever time its access token has left
  (about 8 hours after Claude Code last renewed it, often minutes), and only a
  warm-up renewed it, once one was due. Piaz Switch now renews a parked login
  itself, the way Claude Code does, before the next read after its token lapses,
  so its limits keep showing and the automatic switch can count on it. The
  login in use is never touched: Claude Code renews that one. A login Anthropic
  no longer renews asks you to sign in again instead of saying it will renew.

## [0.5.4] - 2026-10-31

### Fixed
- Adding an Antigravity account no longer ends with "The sign-in did not finish"
  (#4). Since Antigravity 2.0 Google ships two apps: Antigravity IDE, the editor,
  and the Antigravity 2 app, which installs where the editor used to be and keeps
  its Google login in Windows Credential Manager (the Keychain on macOS). Piaz
  Switch opened that app to sign in, and the login it waits for never came. It now
  signs in, switches and reads limits with Antigravity IDE (or the editor before
  2.0), in the data folder the IDE really uses, and never opens, closes or signs in
  to the Antigravity 2 app. Install Antigravity IDE to add Antigravity accounts;
  the Add dialog says so when it isn't there.
- A folder the Antigravity 2 installer put its app into, beside an older editor,
  runs that app from every program in it, so it doesn't count as Antigravity IDE.
  Where that app goes by the same program name as your editor, Piaz Switch asks
  you to close the editor yourself instead of closing it by name.
- Switching a Cursor or Antigravity account while that editor isn't on the
  computer says so straight away, instead of offering to restart it.

## [0.5.3] - 2026-10-27

### Fixed
- A limit nobody has used yet says **not started** instead of looking unread.
  Between sessions, a Claude Code account's 5-hour limit showed 0% with nothing
  beside it: Anthropic sends no reset until the first message starts the period.
  An unused Codex account counted down to resets that never came ("resets in
  4h 59m", "6d 23h"), a time OpenAI moves on with every read. Both now say not
  started (hover it for why), in an account's row, in the all-accounts row and in
  `piaz-switch list`; the period, and its reset, start with the first use.

## [0.5.2] - 2026-10-24

### Fixed
- Adding a Codex account on Windows with the Codex app from the Microsoft Store
  no longer fails with "Access is denied (os error 5)". On some machines Windows
  lets only the Codex app run the `codex` inside its package; Piaz Switch now
  signs in with the copy the app makes of it for itself, in
  `%LOCALAPPDATA%\OpenAI\Codex\bin`.
- Claude Code is found, and Claude Code accounts can be added, on a machine with
  only the Claude app or the Claude Code extension of VS Code (or Cursor,
  Windsurf…): Piaz Switch signs in with the `claude` they carry. Switching
  changes the account Claude Code uses; the Claude app keeps its own sign-in.
- A `claude` or `codex` command of your own still comes first. On macOS the
  Codex app's `codex` is also looked for where the app runs it from.

## [0.5.1] - 2026-10-19

### Fixed
- Adding a Codex account, and **Sign in again**, work on Windows with only the
  Codex app from the Microsoft Store: Piaz Switch now signs in with the `codex`
  the app carries inside it, where before it said Codex wasn't on the machine
  (#3). A `codex` command of its own, if there is one, still comes first. On
  macOS it also looks inside `Codex.app`.

## [0.5.0] - 2026-10-18

### Added
- Cursor and Antigravity (Google's editor for Gemini) accounts, beside Claude Code
  and Codex: add, switch, rename, forget, and see each account's limits.
  - Adding a Cursor account opens Cursor's own sign-in page in the browser, as
    Cursor's command line does; adding an Antigravity account opens Antigravity
    itself in a separate window to sign in to Google there. The editor in use is
    not touched.
  - Both read their account only when they start, so switching one that is open
    asks first, closes it, switches and opens it again (save your work first). A
    login is never written while the editor is open.
  - Cursor shows its plan as Cursor's site names it (Plus, Pro…) and the share
    of it used this period: a week on the weekly plans such as Plus, a month on
    the older ones. Antigravity shows the per-model quota it last read itself.
    Piaz Switch never calls Google.
- Each tool's mark (Claude's spark, the Codex, Cursor and Antigravity marks) beside
  its name in the account list, the Add Account dialog and the Usage pane. The
  dialog picks a tool from tiles.
- An **All accounts together** row at the top of each tool's group with two or
  more accounts: each limit's use and even pace over all of them at once, so it
  shows whether the accounts as a whole are ahead of or behind pace. Each account
  weighs by its plan's size (a Max 20x counts 20 times a Pro; a plan without a
  ×N counts once). The row names the accounts it adds up, any it leaves out, and
  each plan's weight.

## [0.4.1] - 2026-10-18

### Changed
- Adding an account is lighter. The large "add a second account" card is gone,
  and each tool's group has a small **Add** button that opens the dialog with
  that tool chosen. The dialog fills in the next name of a numbered series
  (ben1, ben2 → ben3), ready to accept with Return. It shows the tool choice
  only when there is one, and has shorter copy.

### Fixed
- On a slow or patchy network, usage no longer fails with
  "network: timeout: global": a read gets 30 seconds, one that got no answer is
  tried once more, and one that still fails says the network didn't answer.
- A warm-up that couldn't reach the service (offline, timed out) no longer uses
  one of the day's three tries, so a night without network doesn't leave parked
  logins expired all day. Retries wait 10, 20, 40 and then 60 minutes.

## [0.4.0] - 2026-10-17

### Added
- A download page at <https://piaz-switch.vercel.app> (source in `site/`).
- The author and the project's links (website, GitHub, YouTube @benpiaz, X
  @benpiazai) in **Settings → About** and in a **Made by Ben Piaz** submenu of
  the tray menu; the sidebar says "by Ben Piaz" under the version.

### Changed
- The command line is now `piaz-switch` (it was `pitstop`). The Windows portable
  zip holds `Piaz Switch.exe` and `piaz-switch.exe`, and the macOS command line
  ships as `piaz-switch-cli-macos-<arch>.tar.gz`. Messages and help say
  Piaz Switch throughout. Parked logins stay where they are (`~/.pitstop`), so
  nothing needs adding again. With the portable zip and **Open at login** on,
  turn that setting off and on again once in the new copy, so it starts
  `Piaz Switch.exe` instead of the old `pitstop-app.exe`.
- Usage meters are easier to read: a deeper track with quarter marks, fills
  that glow in their colour, the use ahead of an even pace striped red, a
  clearer pace tick, a bolder figure and pace shown as a green or red pill.

## [0.3.0] - 2026-10-16

### Added
- Updates: the app looks for a new version every few hours (Settings → Updates,
  on by default) and says when one is out, with a notification and an item at the
  top of the tray menu. **Install and Restart** downloads it, checks it against
  the project's signing key, installs it and starts it again. The portable zip
  opens the download page instead. Releases now publish `latest.json` and signed
  update files for this.

### Fixed
- Saving a login, `state.json` or the settings no longer deletes the old file
  before the new one is in place on Windows, so a crash or a busy file can't lose
  it; a failed save leaves no temporary copy of a credential behind.
- A warm-up that runs past two minutes now stops Claude Code and everything it
  started, instead of leaving the app's automation (and switching, for a parked
  account) stuck until a restart.
- Claude Code or Codex installed while the app is open can be added straight
  away: coming back to the window looks for them again.
- A warm-up right after an automatic switch uses the account it is meant for.
- On macOS the app asks the Keychain for its key once per run instead of on
  every usage read, and no longer starts a login shell every minute when no
  account has warm-up on.
- `~/.claude.json` caught mid-write by Claude Code is read again rather than
  taken as a login without an account, which could later drop `oauthAccount`;
  a switch writes nothing if the file can't be read, and a byte-order mark is
  accepted.
- When the app starts before the network is up (at login), usage is read again
  after a minute instead of five.
- Clicks no longer get lost when the window refreshes during the click.
- A sign-in in progress is no longer hidden behind another dialog and left
  running; a code that fails to send ends it. Enter can't start a sign-in for a
  tool that isn't installed.
- Focus returns to where it was when a dialog closes.
- Chart axis labels show the decimals they need ($7.5, not $8), "1 day ago",
  "1 log file", and more errors are shown in Vietnamese.
- Opening the Usage pane with many session logs is faster.
- Warm-up on macOS: the parked login is lent to a private directory whose path is
  resolved, so Claude Code finds it in the keychain and its renewed token comes
  back to the vault.
- While a parked account is being warmed up, its row says so, and switching to it
  (the **Use** button, Enter, the row menu, a suggestion or the tray menu) says to
  wait a few seconds instead of hanging without a word.
- Each account's warm-up status updates as soon as its own warm-up ends, and a round
  of warm-ups that fails midway no longer leaves an account shown as warming up.
- "Token expired · the warm-up renews it" is no longer shown when the warm-up
  can't run for that account (login refused, or three failed tries today).
- A usage reading kept through a failed read (rate limited, offline) stands in for
  at most 15 minutes, and "Updated at" keeps the time of the reading actually
  shown, so old numbers no longer look fresh.
- A usage read asked for less than 30 seconds after the last one is postponed
  rather than dropped: Refresh, Try Again and the reads after a switch or a
  warm-up always happen, and Refresh spins while one waits its turn. A newly
  added account is still read right away.
- Coming back to the window rereads usage at most once a minute, also while the
  usage service is turning reads away.

## [0.2.0] - 2026-10-16

### Added
- `pitstop-app`: a tray / menu bar app with a window to manage accounts. The menu
  lists accounts by tool with a check on the one in use, their plan and usage, and
  suggests a switch when the account in use runs out of a limit. The window shows
  each account's limits as bars with reset times and pace, and adds, signs in
  again, renames and forgets accounts. English and Vietnamese;
  optional start at login.
- Adding an account through the tool's own sign-in (`claude auth login`,
  `codex login`) in a private directory, so the login in use is never touched:
  **Add Account** in the app, `pitstop enroll <name> --sign-in` on the command line.
- Codex usage (5-hour and weekly limits) in the app and `pitstop list`.
- `pitstop rename`.
- Plans (`Max 20x`, `Pro`…) next to account names.
- A **Usage** pane: the estimated cost at API prices and the tokens Claude Code and
  Codex used over the past 24 hours / 7 / 30 / 90 days, read from their local
  session logs — sessions, a daily chart per tool, totals with cache savings, and a
  breakdown by model or day, filterable by tool; plus every account's limits on a
  **Limits** tab. Prices come from LiteLLM's table, shipped with Pitstop and fetched
  again once a day.
- Automation in the app's Settings: switch Claude Code to the account with the most
  room left when a limit of the account in use reaches a chosen share, and a
  notification when the account in use runs out.
- Warm-up: starts a chosen Claude Code account's next five-hour period as soon as the
  last one ends, with one short Haiku message, optionally only between set hours.
- Warm-up also works for parked accounts, and renews a parked login whose access
  token has lapsed, so it is ready when you switch to it.
- Status chips on each account in the window: token expired, warm-up due or
  scheduled, usage temporarily rate-limited.
- **Settings → Appearance**: System, Light or Dark, title bar included.
- Downloads: a Windows installer and portable zip, and `.dmg`s for Apple Silicon
  and Intel Macs.

### Changed
- The app is shown as **Piaz Switch** (window, tray and start-at-login entry); the
  `pitstop` CLI, `~/.pitstop` and keychain names stay as they were.
- The window's look: near-neutral surfaces in light and dark, device-pixel
  hairlines on high-density screens, a skeleton while the first read runs, and copy
  buttons that confirm in place.
- A switch parks the live login under the account it belongs to, told by its
  account and organization (or ChatGPT workspace), instead of whichever account the
  state last recorded as active. A login Pitstop doesn't manage is replaced, never
  parked under another account's name, and the command line says so.
- `enroll` refuses a login that is already enrolled, and account names that differ
  only in case (they would share one vault file on Windows and macOS).
- `state.json` also records each account's non-secret identity and plan.
- License: MIT with the Commons Clause — free to use, modify and share, but not to
  sell. The project now lives at `github.com/Benpiaz/piaz-switch`, and the macOS
  app is `Piaz Switch.app`.

### Fixed
- Warm-up never ran: it could not read the token from a parked login.
- When the usage endpoint rate-limits, the last good reading stays on screen instead
  of disappearing.

### Removed
- `pitstop-tray`: the app replaces it. Turn on **Open Piaz Switch at login** in the
  app's Settings in place of a startup shortcut to `pitstop-tray`.

## [0.1.0] - 2026-10-14

### Added
- `pitstop` CLI: `enroll`, `list`, `use`, `remove` for Claude Code and Codex CLI accounts.
- Encrypted credential vault (AES-256-GCM, key in Windows Credential Manager / macOS Keychain).
- Atomic switching with rollback on failure.
- Claude Code usage (5-hour and weekly limits) in `pitstop list` and the tray.
- `pitstop-tray`: Windows system tray / macOS menu bar app.
