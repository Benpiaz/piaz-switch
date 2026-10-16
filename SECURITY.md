# Security

Piaz Switch handles sign-in tokens for Claude Code and Codex, so security reports
are taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through
[GitHub's security advisories](https://github.com/Benpiaz/piaz-switch/security/advisories/new)
with the steps to reproduce and the version affected. You will get an answer
within a few days, and credit in the release notes if you want it.

Never include real tokens or credential files in a report.

## What Piaz Switch does with your logins

- Parked logins are encrypted with AES-256-GCM in `~/.pitstop/vault/`; the key is
  kept in Windows Credential Manager or the macOS Keychain.
- A login is only ever sent to the service that issued it (Anthropic or OpenAI),
  to read that account's usage. There is no Piaz Switch server and no telemetry.
- The only other request is a daily download of LiteLLM's public price table from
  GitHub; nothing is sent with it.

## Supported versions

Only the latest release gets fixes.
