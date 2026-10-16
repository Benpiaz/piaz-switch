# Contributing

Thanks for helping. Bug reports, ideas and pull requests are welcome — in English
or Vietnamese.

By contributing you agree that your contribution is licensed under this project's
[license](LICENSE) (MIT with the Commons Clause).

## Reporting a bug

Open an [issue](https://github.com/Benpiaz/piaz-switch/issues/new/choose) with your
OS, the app version (Settings), which tool (Claude Code / Codex) and what you
expected. **Never paste tokens, `.credentials.json`, `auth.json` or anything from
`~/.pitstop/vault/`.** Security problems go through [SECURITY.md](SECURITY.md)
instead.

## Development

Layout:

| Crate | What |
|---|---|
| `crates/pitstop-core` | accounts, vault, switching, usage, the Usage pane's log reader |
| `crates/pitstop-cli` | the `pitstop` command line |
| `crates/pitstop-app` | the Tauri 2 app (tray + window); UI in `ui/` |

```sh
cargo build -p pitstop-app
PITSTOP_HOME=/tmp/sandbox cargo run -p pitstop-app   # against a sandbox, not your real accounts
```

The window's CSS is Tailwind v4: edit `crates/pitstop-app/ui-src/app.css`, then run
`npm install && npm run build` in `crates/pitstop-app/ui-src`; commit the built
`ui/app.css` too. Dark-theme rules use `:root[data-theme="dark"]`.

Before opening a pull request, run what CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat(app): …`, `fix(core): …`), and user-visible changes get a line in
[CHANGELOG.md](CHANGELOG.md).

## Releasing

Bump `version` in the root `Cargo.toml` (the app takes its version from there),
date the section in `CHANGELOG.md`, then push a tag `vX.Y.Z`. The Release workflow
builds the Windows installer and portable zip, the macOS `.dmg`s, and publishes
them with the changelog section as notes.

## macOS checklist (first run on a Mac)

- [ ] `cargo test --workspace` — includes Unix-only file-permission tests
- [ ] `cargo test -p pitstop-core real_claude -- --ignored` — the private sign-in
      prints its address and leaves the login in use alone
- [ ] `Piaz Switch.app` appears in the menu bar with no Dock icon; the Dock icon
      shows only while its window is open
- [ ] The menu lists accounts with usage; choosing one swaps the Keychain item and
      `~/.claude.json#oauthAccount`
- [ ] **Add Account** for Claude Code: the browser sign-in works, the new account is
      parked, and the Keychain item `Claude Code-credentials-<hash>` it created is gone
- [ ] **Open Piaz Switch at login** adds a LaunchAgent and starts the app in the menu bar
