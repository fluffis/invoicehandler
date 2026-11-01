# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Cargo workspace containing two Rust daemons that work together as a pipeline:

1. **invoicefetcher** — Monitors an email inbox via IMAP IDLE, matches emails against configurable rules (sender/subject), extracts links from matched emails, opens them in a headless browser (for JS-rendered pages), finds download links via CSS selectors, and downloads files to a target directory.

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
handler/                      # invoicehandler binary
  Cargo.toml
  src/main.rs                 # Single-file application (~260 lines)
fetcher/                      # invoicefetcher binary
  Cargo.toml
  src/
    main.rs                   # Entry point, orchestration loop
    config.rs                 # Config loading, Settings/Rule structs
    imap.rs                   # IMAP connection, IDLE loop, email fetching
    rules.rs                  # Email rule matching, link extraction
    browser.rs                # Headless Chrome automation, file download
```

## Architecture

### invoicehandler (handler/)

Single-file application. Key flow:

1. **Config loading** — `load_settings()` and `load_rules()` parse `~/.invoicehandler` (Linux) via `rust-ini`
2. **File watching** — `notify` crate's `RecommendedWatcher` monitors both the watch directory and the config file itself
3. **Rename logic** — `apply_rename()` matches filenames against ordered regex rules; first match wins
4. **Lock handling** — `wait_for_file_unlock()` retries opening files that may still be written to
5. **Hot reload** — config file changes trigger `load_rules()` reload without restarting

### invoicefetcher (fetcher/)

Async application (tokio). Key flow:

1. **Config loading** — parses `~/.invoicefetcher` (Linux) with `[imap]`, `[settings]`, and `[rule.*]` sections
2. **Browser launch** — starts headless Chrome via `chromiumoxide`, keeps it running for reuse
3. **IMAP IDLE loop** — connects to mailbox, enters IDLE mode, wakes on new email
4. **Rule matching** — matches email sender (substring) and subject (regex) against configured rules
5. **Link extraction** — regex with capture group extracts URL from email HTML body
6. **Browser navigation** — opens URL, waits for CSS selector, extracts download link
7. **File download** — downloads file via reqwest to output directory
8. **Seen flag** — marks processed emails as `\Seen` via IMAP STORE
9. **Reconnection** — outer loop handles IMAP disconnections with configurable delay

## Config Files

- **Handler**: `~/.invoicehandler` on Linux, `~/Library/Application Support/invoicehandler/config.ini` on macOS, `%APPDATA%\invoicehandler\config.ini` on Windows
- **Fetcher**: `~/.invoicefetcher` on Linux, similar platform-conditional paths on macOS/Windows

## Dependencies

Shared (workspace-level): `dirs` 5, `regex` 1, `rust-ini` 0.21

Handler-specific: `notify` 6, `notify-rust` 4

Fetcher-specific: `tokio` 1, `async-imap` 0.12, `async-native-tls` 0.6, `chromiumoxide` 0.9, `mail-parser` 0.9, `reqwest` 0.12, `futures` 0.3, `log` 0.4, `env_logger` 0.11
