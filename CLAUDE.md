# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Cargo workspace containing two Rust daemons that work together as a pipeline:

1. **invoicefetcher** — Monitors an email inbox via IMAP IDLE, matches emails against configurable rules (sender/subject), and either extracts links to open in a headless browser for download, or saves email attachments directly. Supports Google OAuth2 for Gmail. Runs as a systemd user service on Linux.

2. **invoicehandler** — Watches a directory and automatically renames files based on configurable regex patterns. Runs as a systemd user service on Linux.

The typical flow: email arrives → invoicefetcher downloads the attachment/invoice → places it in the watch directory → invoicehandler renames it.

## Build & Run

```bash
cargo build                              # Build both binaries (debug)
cargo build --release                    # Build both binaries (release)
cargo build -p invoicehandler            # Build handler only
cargo build -p invoicefetcher            # Build fetcher only
cargo run -p invoicehandler              # Run handler
cargo run -p invoicefetcher              # Run fetcher
./install-handler.sh                     # Build + install handler as systemd service
./install-fetcher.sh                     # Build + install fetcher as systemd service
```

No test suite exists yet. No linter/formatter configured beyond standard `cargo fmt` and `cargo clippy`.

## Workspace Structure

```
Cargo.toml                    # Workspace manifest (shared deps: dirs, regex, rust-ini)
RULES.md                      # Detailed fetcher rule documentation
config.ini.example            # Handler config example
config.ini.fetcher.example    # Fetcher config example
rsyslog-invoice.conf          # rsyslog config to redirect logs to /var/log/invoice.log
logrotate-invoice.conf        # logrotate config for /var/log/invoice.log
install-handler.sh            # Handler systemd install script (upgrade-safe)
install-fetcher.sh            # Fetcher systemd install script (upgrade-safe)
handler/                      # invoicehandler binary
  Cargo.toml
  src/main.rs                 # Single-file application (~260 lines)
fetcher/                      # invoicefetcher binary
  Cargo.toml
  src/
    main.rs                   # Entry point, CLI flags (--auth, --replay), logging setup
    config.rs                 # Config loading, Settings/Rule/RuleAction structs
    imap.rs                   # IMAP connection, IDLE loop, replay mode, message processing
    rules.rs                  # Email rule matching (browser download + attachment save)
    browser.rs                # Headless Chrome automation, CDP download events, file download
    oauth.rs                  # Google OAuth2 flow (authorization, token refresh, storage)
```

## Architecture

### invoicehandler (handler/)

Single-file application. Key flow:

1. **Config loading** — `load_settings()` and `load_rules()` parse `~/.invoicehandler` (Linux) via `rust-ini`
2. **File watching** — `notify` crate's `RecommendedWatcher` monitors both the watch directory and the config file itself
3. **Rename logic** — `apply_rename()` matches filenames against ordered regex rules; first match wins
4. **Lock handling** — `wait_for_file_unlock()` retries opening files that may still be written to
5. **Hot reload** — config file changes trigger `load_rules()` reload without restarting
6. **Logging** — syslog via `log` + `syslog` crates (process name: `invoicehandler`)
7. **Notifications** — D-Bus desktop notifications via `notify-rust` on rename success/failure

### invoicefetcher (fetcher/)

Async application (tokio). Key flow:

1. **Config loading** — parses `~/.invoicefetcher` (Linux) with `[imap]`, `[settings]`, and `[rule.*]` sections
2. **Authentication** — password or Google OAuth2 (XOAUTH2 SASL). OAuth tokens stored in `~/.invoicefetcher.oauth_token`
3. **Browser launch** — starts Chrome via `chromiumoxide`; headless by default, visible when `browser_headless = false`
4. **IMAP IDLE loop** — connects to mailbox, enters IDLE mode, wakes on new email or 29-minute timeout
5. **Hot reload** — rules are reloaded from config at the start of each IDLE cycle
6. **Rule matching** — two rule types:
   - **Browser download**: sender/subject match → extract URL from body via regex → open in browser → click CSS selector → wait for CDP download event
   - **Attachment save**: sender/subject match → find attachment by filename regex → save directly
7. **UID tracking** — tracks last processed UID in `~/.invoicefetcher.uid`; emails are never marked as read (`BODY.PEEK[]`)
8. **Post-processing** — successfully processed emails are moved to `processed_folder` (e.g. Gmail label) if configured
9. **Notifications** — D-Bus desktop notifications on download/save success or failure
10. **Reconnection** — outer loop handles IMAP disconnections with configurable delay
11. **Temp cleanup** — files in `/tmp/invoicefetcher_downloads/` older than 24h are removed on browser launch

### Browser automation details (browser.rs)

- CDP `SetDownloadBehavior` is set per-page with `AllowAndName` + events enabled
- `EventDownloadWillBegin` captures the suggested filename (avoids GUID filenames)
- `EventDownloadProgress` tracks download completion
- Selector polling (500ms interval) handles JS-rendered content that appears after page load
- Falls back to detecting new tabs if click opens a new window instead of triggering a download
- Debug HTML dumps saved to `/tmp/invoicefetcher_downloads/` when debug logging is enabled

### IMAP connection details (imap.rs)

- Server greeting must be consumed before `authenticate()` — `async-imap`'s `Client::new` does not read it
- OAuth2 uses the `Authenticator` trait with XOAUTH2 SASL format
- `uid_search`/`uid_fetch` used throughout (not sequence numbers)
- First run records the current max UID and skips existing messages

## CLI Flags (invoicefetcher)

```bash
invoicefetcher                           # Normal service mode (IMAP IDLE loop)
invoicefetcher --auth                    # Interactive OAuth2 authorization flow
invoicefetcher --replay 121297           # Replay specific UID
invoicefetcher --replay 121297,121298    # Replay multiple UIDs
invoicefetcher --replay last:10          # Replay last N messages
invoicefetcher --replay from:user@x.com  # Replay by sender (IMAP SEARCH)
invoicefetcher --replay subject:invoice  # Replay by subject (IMAP SEARCH)
```

Replay mode: opens its own IMAP connection, logs to both syslog and stderr at debug level, does not move emails or update the UID watermark. Safe to run alongside the service.

## Config Files

- **Handler**: `~/.invoicehandler` on Linux, `~/Library/Application Support/invoicehandler/config.ini` on macOS, `%APPDATA%\invoicehandler\config.ini` on Windows
- **Fetcher**: `~/.invoicefetcher` on Linux, similar platform-conditional paths on macOS/Windows
- **Fetcher UID state**: `~/.invoicefetcher.uid`
- **Fetcher OAuth token**: `~/.invoicefetcher.oauth_token` (mode 0600)
- **Rule documentation**: see `RULES.md` for detailed rule configuration reference

## Logging

Both binaries log to syslog (facility `LOG_USER`) with distinct process names (`invoicehandler`, `invoicefetcher`).

```bash
# View logs via journald
journalctl --user -u invoicehandler -f
journalctl --user -u invoicefetcher -f

# Redirect to dedicated file (install rsyslog config)
sudo cp rsyslog-invoice.conf /etc/rsyslog.d/invoice.conf
sudo cp logrotate-invoice.conf /etc/logrotate.d/invoice
sudo systemctl restart rsyslog
tail -f /var/log/invoice.log
```

## Install Scripts

Both `install-handler.sh` and `install-fetcher.sh` are upgrade-safe:
- Check if the service is running before install
- Stop it, build and install the new binary, restart if it was running
- Adapt output messaging for first install vs upgrade

## Dependencies

Shared (workspace-level): `dirs` 5, `regex` 1, `rust-ini` 0.21

Handler-specific: `notify` 6, `notify-rust` 4, `log` 0.4, `syslog` 7

Fetcher-specific: `tokio` 1, `async-imap` 0.12, `async-native-tls` 0.6, `chromiumoxide` 0.9, `mail-parser` 0.9, `reqwest` 0.12, `futures` 0.3, `log` 0.4, `syslog` 7, `serde` 1, `serde_json` 1, `open` 5, `notify-rust` 4
