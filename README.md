# Sysaidmin

SYSAIDMIN is like if you squished Claude Code down so you felt okay `wget`-ing it onto your server.
It's got planning logic, a terminal UI, and some partially-tested safety features (YMMV).

## Quick start

```bash
cargo build

# run requires SYSAIDMIN_API_KEY or config file
cargo run -p sysaidmin
```

## Configuration

Create `~/.sysaidmin/config.toml` (or use env vars).

```toml
anthropic_api_key = "sk-ant-..."
default_shell = "/bin/bash"
dry_run = false
offline_mode = false

[allowlist]
command_patterns = ["^(sudo\\s+)?systemctl\\s+", "^journalctl"]
file_patterns = ["^/etc/ssh/.*", "^/var/log/.*"]
max_edit_size_kb = 64
```

Env overrides & runtime options:

- `SYSAIDMIN_API_KEY`, `ANTHROPIC_API_KEY`
- `SYSAIDMIN_DRYRUN=1` to force dry-run mode
- `SYSAIDMIN_SESSION_DIR=/desired/path` to control export location
- `--model <name>` CLI flag overrides the interactive picker and uses the specified model immediately. Without the flag, the app fetches the current Anthropic model list on startup and lets you choose one before launching the TUI.

> **Note:** The config file is parsed as TOML; string values (like API keys) **must** be quoted (`"sk-..."`). Unquoted keys will be rejected with a parse error that points to the config file.
  **Double Note:** I just use ENV vars, don't use the config unless you need it

## Features

- **Structured plans**: The LLM returns JSON worklists; allowlist rules gate each task.
- **Automatic execution**: As soon as a plan arrives, every allowlisted task runs automatically (commands then file edits). File edits get automatic `*.sysaidmin.bak` backups, while blocked tasks stay highlighted for review.
- **Dry-run mode**: When enabled, commands and edits are simulated but logged for review.
- **Session exports**: Every plan snapshot is written to JSON, and logs stream to `~/.local/share/sysaidmin`.
- **Packaging**: `cargo-deb` metadata ships a single `/usr/bin/sysaidmin` binary ready for Debian-based systems.

## Packaging via Docker

Just run `./build.sh` and it'll create the full matrix of `.deb` packages and binaries for each popular architecture.
