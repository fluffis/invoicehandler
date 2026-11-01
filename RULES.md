# Invoicefetcher Rules Reference

Rules tell invoicefetcher which emails to act on and how to extract files from them. Each rule is defined as an INI section in the config file (`~/.invoicefetcher` on Linux).

## Rule basics

Each rule is a section named `[rule.<name>]` where `<name>` is an arbitrary identifier used in log messages.

```ini
[rule.my_provider]
sender = billing@example.com
```

Rules are evaluated in the order they appear in the config file. **The first matching rule wins** -- once a rule matches an email and its action succeeds, no further rules are tried for that email.

A rule must have either `attachment_pattern` or `link_pattern` (not both). If neither is present, the rule is skipped with a warning at startup.

## Common fields

These fields apply to both rule types.

### sender (required)

Case-insensitive substring match against the email's `From` address.

```ini
sender = billing@example.com
```

The value `billing@example.com` will match:
- `billing@example.com`
- `Billing Department <billing@example.com>`
- `BILLING@EXAMPLE.COM`

It will **not** match:
- `billing@other-example.com` (different domain)
- `nobilling@example.com` (unless the substring happens to match)

This is a plain substring match, not a regex. Keep it specific enough to avoid false positives.

### subject_pattern (optional)

A regex pattern matched against the email's `Subject` header. If omitted, any subject matches (only `sender` is checked).

```ini
subject_pattern = Invoice|Receipt
```

Standard Rust regex syntax. Some useful patterns:

| Pattern | Matches |
|---|---|
| `Invoice` | Subject contains "Invoice" (case-sensitive) |
| `(?i)invoice` | Subject contains "invoice" (case-insensitive) |
| `Invoice #\d+` | "Invoice #" followed by digits |
| `Invoice\|Receipt` | Either "Invoice" or "Receipt" |
| `^Your monthly invoice$` | Exact subject match |

Note: the match is a search (finds the pattern anywhere in the subject), not a full-string match. Use `^` and `$` anchors if you need an exact match.

## Rule type 1: Browser download

For emails that contain a link to a web page where you need to click a button to download a file. This is common with services like Stripe, OpenAI, and other SaaS providers that send "Your invoice is ready" emails with a link to their portal.

### Required fields

#### link_pattern

A regex with **one capture group** that extracts the URL from the email's HTML body. Capture group 1 must be the URL.

```ini
link_pattern = href="(https://example\.com/invoice/[^"]+)"
```

The regex runs against the raw HTML source of the email body. Common patterns:

```ini
# Match a specific domain link inside an href attribute
link_pattern = href="(https://billing\.stripe\.com/[^"]+)"

# Match a tracking/redirect URL
link_pattern = href="(https://email\.example\.com/click\?[^"]+)"

# Match any link containing "invoice" in the path
link_pattern = href="(https://[^"]*invoice[^"]*)"
```

Tips:
- The email HTML body often contains tracking URLs, not direct links. Check the actual email source (saved to `/tmp/invoicefetcher_downloads/email_uid_<N>.html` in debug mode) to see what URLs are present.
- HTML entities like `&amp;` appear in the raw source. Your regex sees the raw HTML, so match `&amp;` not `&` if needed.
- If the link_pattern matches but has no capture group 1, the rule is skipped with a debug log message.

#### download_selector

A CSS selector for the element to **click** on the browser-rendered page to trigger the file download.

```ini
download_selector = a.download-btn
```

This uses standard CSS selector syntax via `document.querySelector()`. The first matching element is clicked. Examples:

```ini
# By class
download_selector = a.download-btn

# By ID
download_selector = #download-button

# Nested selector
download_selector = .InvoiceDetailsRow-Container > button

# By attribute
download_selector = a[download]

# By text content (not directly supported -- use a class or structure-based selector)
# Combine structure to narrow down
download_selector = .invoice-actions button:first-child
```

Tips:
- Set `browser_headless = false` in `[settings]` to open a visible browser window with DevTools. You can then inspect the page and test selectors in the console.
- The fetcher polls for the selector every 500ms after page load, waiting up to `browser_timeout_secs`. This handles JS-rendered content that appears after the initial page load.
- If multiple elements match, the first one is clicked.
- After a successful click, the fetcher waits for Chrome's CDP download events. The downloaded file is saved with its original suggested filename (e.g. `Invoice-2026-10.pdf`), not a GUID.

### Full example

```ini
[rule.openai]
sender = receipts@openai.com
subject_pattern = (?i)receipt
link_pattern = href="(https?://[^"]*openai[^"]*)"
download_selector = .InvoiceDetailsRow-Container > button
```

### How it works

1. Email arrives, sender and subject match
2. `link_pattern` extracts a URL from the email HTML body
3. The URL is opened in a headless (or visible) Chrome browser
4. The fetcher waits for `download_selector` to appear in the DOM (polls every 500ms)
5. The element is clicked
6. The fetcher waits for the browser's download to complete
7. The file is moved to `output_directory` with its original filename

## Rule type 2: Attachment save

For emails that have the invoice/document attached directly (e.g. a PDF or CSV file attached to the email).

### Required fields

#### attachment_pattern

A regex matched against each attachment's filename. The first attachment whose filename matches is saved.

```ini
attachment_pattern = .*\.pdf$
```

Examples:

```ini
# Any PDF attachment
attachment_pattern = .*\.pdf$

# Any PDF or CSV
attachment_pattern = .*\.(pdf|csv)$

# Filename starts with "Invoice"
attachment_pattern = ^Invoice.*\.pdf$

# Specific naming pattern
attachment_pattern = ^INV-\d{4}-\d{2}\.pdf$

# Case-insensitive PDF extension
attachment_pattern = (?i)\.pdf$
```

### Full example

```ini
[rule.vendor_invoices]
sender = accounting@vendor.com
subject_pattern = (?i)monthly invoice
attachment_pattern = .*\.pdf$
```

### How it works

1. Email arrives, sender and subject match
2. The email's MIME attachments are enumerated (filename and binary content)
3. Each attachment's filename is tested against `attachment_pattern`
4. The first matching attachment is saved directly to `output_directory` with its original filename
5. No browser is involved

## Evaluation order

For each new email, rules are evaluated top-to-bottom as they appear in the config file:

```
For each rule (in config order):
  1. Does sender match? (case-insensitive substring)
     No  -> skip to next rule
     Yes -> continue

  2. Is subject_pattern set?
     Yes -> does it match the subject? (regex search)
       No  -> skip to next rule
       Yes -> continue
     No  -> continue (any subject matches)

  3. Perform the rule's action:
     Browser download:
       - Search email HTML body with link_pattern
       - If no match -> skip to next rule
       - If match -> open URL, click selector, download file
     Attachment save:
       - Check each attachment filename against attachment_pattern
       - If no match -> skip to next rule
       - If match -> save the attachment

  4. If action succeeded:
     - Move email to processed_folder (if configured)
     - Stop processing this email (first match wins)
```

## Post-processing

### processed_folder

When a rule matches and the file is successfully saved, the email can be moved to an IMAP folder:

```ini
[settings]
processed_folder = Ekonomi
```

- **Gmail**: use the label name directly (e.g. `Ekonomi`, not `INBOX.Ekonomi`)
- **Other servers**: use the full folder path as your server expects (e.g. `INBOX.Ekonomi` or `INBOX/Ekonomi`)
- **If unset**: emails are left in place after processing

The move only happens on success. If the download or save fails, the email stays in the inbox and will be retried on the next cycle.

### Email state preservation

- **Unmatched emails**: left completely untouched. No flags are modified, the email is not moved.
- **Matched but failed**: left in place, no flags modified. The UID watermark still advances, so it won't be retried automatically (reset the UID file to reprocess).
- **Matched and succeeded**: moved to `processed_folder` if configured, otherwise left in place.

The fetcher never sets the `\Seen` (read) flag on any email. It uses `BODY.PEEK[]` to fetch message content without side effects.

## Debugging rules

### Enable debug logging

```bash
RUST_LOG=debug invoicefetcher
```

This logs every step of rule evaluation:
```
DEBUG Rule 'openai': checking sender='receipts@openai.com' against from='receipts@openai.com'
DEBUG Rule 'openai': sender matched
DEBUG Rule 'openai': subject 'Your receipt from OpenAI' matched
DEBUG Rule 'openai': searching body (4521 bytes) with link_pattern '...'
DEBUG Rule 'openai': link_pattern matched, URL: https://...
```

### Inspect email HTML

With debug logging enabled, each email's HTML body is saved to `/tmp/invoicefetcher_downloads/email_uid_<UID>.html`. Open this file to see exactly what `link_pattern` runs against.

### Visible browser

Set `browser_headless = false` in `[settings]` to open Chrome with DevTools. You can inspect the rendered page and test CSS selectors in the console.

### Reprocess old emails

The fetcher tracks the last processed UID in `~/.invoicefetcher.uid`. To reprocess emails, set this to a lower value:

```bash
# Reprocess from a specific UID
echo 12000 > ~/.invoicefetcher.uid

# Reprocess from the beginning (will skip all on first run by default)
echo 0 > ~/.invoicefetcher.uid
```

## Complete config example

```ini
[imap]
server = imap.gmail.com
port = 993
username = user@example.com
auth_method = oauth2
client_id = 123456789.apps.googleusercontent.com
client_secret = GOCSPX-xxxxx

[settings]
output_directory = /home/user/Documents/invoices
reconnect_delay_secs = 30
browser_timeout_secs = 30
browser_headless = true
processed_folder = Ekonomi

# OpenAI receipts: click "Download invoice" on their Stripe page
[rule.openai]
sender = receipts@openai.com
link_pattern = href="(https?://[^"]*openai[^"]*invoice[^"]*)"
download_selector = .InvoiceDetailsRow-Container > button

# AWS invoices: PDF attached directly to the email
[rule.aws]
sender = no-reply@amazon.com
subject_pattern = (?i)invoice available
attachment_pattern = .*\.pdf$

# Vendor with monthly CSV reports
[rule.vendor_reports]
sender = reports@vendor.com
subject_pattern = ^Monthly Report
attachment_pattern = ^report_\d{4}_\d{2}\.csv$

# Generic catch-all for any PDF from the accounting team
[rule.accounting_pdfs]
sender = accounting@mycompany.com
attachment_pattern = \.pdf$
```
