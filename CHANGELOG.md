# Changelog

## [Unreleased]

### Fixed
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
