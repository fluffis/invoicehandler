# invoicehandler

A file watcher that automatically renames files based on configurable regex patterns.

## Building

```bash
cargo build --release
```

## Installation (Linux)

The install script builds the binary, installs it to `~/.local/bin`, and creates a systemd user service.

```bash
./install.sh
```

After installation, create your config file at `~/.invoicehandler`, then enable and start the service:

```bash
systemctl --user enable invoicehandler.service
systemctl --user start invoicehandler.service
```

### Service management

```bash
systemctl --user status invoicehandler.service  # Check status
systemctl --user restart invoicehandler.service # Restart after config changes
journalctl --user -u invoicehandler.service     # View logs
```

## Configuration

Create a config file at the appropriate location for your platform:

| Platform | Config Location |
|----------|-----------------|
| Linux | `~/.invoicehandler` |
| macOS | `~/Library/Application Support/invoicehandler/config.ini` |
| Windows | `%APPDATA%\invoicehandler\config.ini` |

### Config file format

```ini
[settings]
watch_directory = /path/to/watch
max_lock_retries = 30
lock_retry_delay_ms = 1000

[translations]
# Format: regex_pattern = replacement_string
# Uses Rust regex syntax with capture groups
#
# Examples:
# invoice_(\d{4})_(\\d{2})_(\\d{2})_(.+)\\.pdf = Invoice_$4_$1-$2-$3.pdf
# invoice_acme_(.+)\\.pdf = Acme_Corp_Invoice_$1.pdf
```

### Settings

- `watch_directory` - Directory to monitor for new files
- `max_lock_retries` - Number of attempts to access a locked file (default: 30)
- `lock_retry_delay_ms` - Delay between retry attempts in milliseconds (default: 1000)

### Translation rules

Each rule is a regex pattern mapped to a replacement string. Capture groups (`$1`, `$2`, etc.) can be used in the replacement string.

## Usage

```bash
./invoicehandler
```

The program watches the configured directory and automatically renames files matching any translation rule. The config file is also watched and rules are reloaded when it changes.
