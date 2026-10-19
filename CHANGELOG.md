# Changelog

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
