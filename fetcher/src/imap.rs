use async_imap::extensions::idle::IdleResponse;
use async_imap::{Authenticator, Session};
use async_native_tls::TlsStream;
use futures::TryStreamExt;
use mail_parser::MessageParser;
use std::path::Path;
use std::time::Duration;
use tokio::net::TcpStream;

use mail_parser::MimeHeaders;

use notify_rust::{Notification, Urgency};

use crate::config::{AuthMethod, ImapSettings, Rule, Settings};
use crate::rules::{self, Attachment, Email, MatchResult};

type ImapSession = Session<TlsStream<TcpStream>>;

struct OAuth2Auth {
    user: String,
    access_token: String,
}

impl Authenticator for &OAuth2Auth {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}

pub async fn connect(settings: &ImapSettings) -> Result<ImapSession, Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", settings.server, settings.port);
    log::debug!("TCP connecting to {}...", addr);
    let tcp = TcpStream::connect(&addr).await?;
    log::debug!("TCP connected, starting TLS handshake...");

    let tls = async_native_tls::TlsConnector::new();
    let tls_stream = tls.connect(&settings.server, tcp).await?;
    log::debug!("TLS handshake complete");

    let mut client = async_imap::Client::new(tls_stream);

    // Consume the server greeting before authenticating.
    // Client::new doesn't read it, and authenticate()'s handshake
    // doesn't expect it — it would stall waiting for the continuation
    // challenge while the greeting sits unread in the buffer.
    log::debug!("Reading server greeting...");
    {
        match client.read_response().await {
            Ok(Some(greeting)) => {
                log::debug!("Server greeting: {:?}", greeting.parsed());
            }
            Ok(None) => {
                return Err("Connection closed before server greeting".into());
            }
            Err(e) => {
                return Err(format!("Failed to read server greeting: {}", e).into());
            }
        }
    }

    let session = match &settings.auth {
        AuthMethod::Password { password } => {
            log::debug!("Authenticating with password for user '{}'...", settings.username);
            client
                .login(&settings.username, password)
                .await
                .map_err(|e| format!("IMAP login failed: {}", e.0))?
        }
        AuthMethod::OAuth2 {
            client_id,
            client_secret,
        } => {
            log::debug!("Obtaining OAuth2 access token...");
            let access_token =
                crate::oauth::get_access_token(client_id, client_secret).await?;
            log::debug!("Authenticating with XOAUTH2 for user '{}'...", settings.username);
            let auth = OAuth2Auth {
                user: settings.username.clone(),
                access_token,
            };
            client
                .authenticate("XOAUTH2", &auth)
                .await
                .map_err(|e| format!("IMAP XOAUTH2 auth failed: {}", e.0))?
        }
    };
    log::debug!("IMAP authentication successful");

    Ok(session)
}

/// Load the last processed UID from the state file.
fn load_last_uid(path: &Path) -> u32 {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}

/// Save the last processed UID to the state file.
fn save_last_uid(path: &Path, uid: u32) {
    if let Err(e) = std::fs::write(path, uid.to_string()) {
        log::error!("Failed to save UID state to {}: {}", path.display(), e);
    }
}

/// Process a single fetched IMAP message against the rules.
/// Returns true if the message was successfully processed (file saved/downloaded).
async fn process_message(
    msg: &async_imap::types::Fetch,
    rules: &[Rule],
    settings: &Settings,
    browser: &crate::browser::BrowserSession,
) -> Option<(u32, bool)> {
    let msg_uid = match msg.uid {
        Some(uid) => uid,
        None => {
            log::warn!("Message has no UID, skipping");
            return None;
        }
    };

    let body = msg.body()?;
    let parsed = MessageParser::default().parse(body)?;

    let from = parsed
        .from()
        .and_then(|f| f.first())
        .and_then(|a| a.address())
        .unwrap_or("")
        .to_string();
    let subject = parsed.subject().unwrap_or("").to_string();
    let body_html = parsed
        .body_html(0)
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Extract attachments
    let attachments: Vec<Attachment> = parsed
        .attachments
        .iter()
        .filter_map(|&part_id| {
            let part = parsed.parts.get(part_id)?;
            let filename = part.attachment_name()?.to_string();
            let data = match &part.body {
                mail_parser::PartType::Binary(d)
                | mail_parser::PartType::InlineBinary(d) => d.to_vec(),
                _ => return None,
            };
            Some(Attachment { filename, data })
        })
        .collect();

    log::info!(
        "Processing UID {}: from='{}', subject='{}', {} attachment(s)",
        msg_uid,
        from,
        subject,
        attachments.len()
    );
    for att in &attachments {
        log::debug!(
            "  Attachment: '{}' ({} bytes)",
            att.filename,
            att.data.len()
        );
    }

    // Dump email body HTML for debugging
    if log::log_enabled!(log::Level::Debug) {
        let dump_dir = std::env::temp_dir().join("invoicefetcher_downloads");
        let dump_path = dump_dir.join(format!("email_uid_{}.html", msg_uid));
        match std::fs::write(&dump_path, &body_html) {
            Ok(_) => log::debug!("Email body HTML saved to: {}", dump_path.display()),
            Err(e) => log::debug!("Failed to save email HTML: {}", e),
        }
    }

    let email = Email {
        from,
        subject,
        body_html,
        attachments,
    };

    let processed = match rules::match_email(&email, rules) {
        Some((rule, MatchResult::BrowserDownload { url, download_selector })) => {
            log::info!(
                "Rule '{}' matched (browser download), URL: {}",
                rule.name,
                url
            );

            match browser
                .fetch_download(&url, &download_selector, &settings.output_directory)
                .await
            {
                Ok(path) => {
                    let fname = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    log::info!("Downloaded file to: {}", path.display());
                    let _ = Notification::new()
                        .summary("Invoice Downloaded")
                        .body(&format!("Rule '{}': {}", rule.name, fname))
                        .urgency(Urgency::Normal)
                        .show();
                    true
                }
                Err(e) => {
                    log::error!("Failed to download from '{}': {}", url, e);
                    let _ = Notification::new()
                        .summary("Invoice Download Failed")
                        .body(&format!("Rule '{}': {}", rule.name, e))
                        .urgency(Urgency::Critical)
                        .show();
                    false
                }
            }
        }
        Some((rule, MatchResult::SaveAttachment { filename, data })) => {
            log::info!(
                "Rule '{}' matched (attachment), saving '{}'",
                rule.name,
                filename
            );
            let output_path = settings.output_directory.join(&filename);
            match std::fs::write(&output_path, &data) {
                Ok(_) => {
                    log::info!("Saved attachment to: {}", output_path.display());
                    let _ = Notification::new()
                        .summary("Invoice Saved")
                        .body(&format!("Rule '{}': {}", rule.name, filename))
                        .urgency(Urgency::Normal)
                        .show();
                    true
                }
                Err(e) => {
                    log::error!("Failed to save attachment '{}': {}", filename, e);
                    let _ = Notification::new()
                        .summary("Invoice Save Failed")
                        .body(&format!("Rule '{}': {}", rule.name, e))
                        .urgency(Urgency::Critical)
                        .show();
                    false
                }
            }
        }
        None => {
            log::debug!("UID {}: no rule matched", msg_uid);
            false
        }
    };

    Some((msg_uid, processed))
}

pub async fn run_idle_loop(
    settings: &Settings,
    initial_rules: Vec<Rule>,
    browser: &crate::browser::BrowserSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rules = initial_rules;
    let uid_state_path = crate::config::get_uid_state_path();
    let mut last_uid = load_last_uid(&uid_state_path);

    let mut session = connect(&settings.imap).await?;
    let mailbox = session.select(&settings.imap.mailbox).await?;

    log::info!(
        "Connected to {}:{}, mailbox: {} ({} messages, UID validity: {})",
        settings.imap.server,
        settings.imap.port,
        settings.imap.mailbox,
        mailbox.exists,
        mailbox.uid_validity.unwrap_or(0),
    );

    // Print the 20 newest email headers for debugging
    if mailbox.exists > 0 {
        let start = mailbox.exists.saturating_sub(19);
        let range = format!("{}:*", start);
        log::debug!("Fetching headers for messages {}...", range);
        match session.uid_fetch(&range, "BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)]").await {
            Ok(fetch_stream) => {
                let headers: Vec<_> = fetch_stream.try_collect().await.unwrap_or_default();
                log::debug!("--- 20 newest messages ---");
                for msg in &headers {
                    let uid = msg.uid.map(|u| u.to_string()).unwrap_or_else(|| "?".into());
                    let header_text = msg.header()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default();
                    let from = header_text.lines()
                        .find(|l| l.to_lowercase().starts_with("from:"))
                        .unwrap_or("From: (unknown)");
                    let subject = header_text.lines()
                        .find(|l| l.to_lowercase().starts_with("subject:"))
                        .unwrap_or("Subject: (none)");
                    let date = header_text.lines()
                        .find(|l| l.to_lowercase().starts_with("date:"))
                        .unwrap_or("Date: (unknown)");
                    log::debug!("  UID {}: {} | {} | {}", uid, from, subject, date);
                }
                log::debug!("--- end ---");
            }
            Err(e) => {
                log::debug!("Could not fetch recent headers: {}", e);
            }
        }
    }

    // On first run (no saved UID), start from the current highest UID
    // so we don't process the entire mailbox history
    if last_uid == 0 {
        let search_result = session.uid_search("ALL").await?;
        if let Some(&max_uid) = search_result.iter().max() {
            last_uid = max_uid;
            save_last_uid(&uid_state_path, last_uid);
            log::info!(
                "First run: starting from UID {}, skipping existing messages",
                last_uid
            );
        } else {
            log::info!("Mailbox is empty, waiting for new messages");
        }
    } else {
        log::info!("Resuming from last processed UID: {}", last_uid);
    }

    loop {
        // Reload rules from config at the start of each cycle
        {
            let config_path = crate::config::get_config_path();
            match ini::Ini::load_from_file(&config_path) {
                Ok(ini) => {
                    let new_rules = crate::config::load_rules(&ini);
                    if new_rules.len() != rules.len() {
                        log::info!("Rules reloaded: {} rule(s)", new_rules.len());
                    }
                    rules = new_rules;
                }
                Err(e) => {
                    log::warn!("Failed to reload config, keeping current rules: {}", e);
                }
            }
        }

        // Search for messages with UID greater than the last one we processed
        let search_query = format!("UID {}:*", last_uid + 1);
        log::debug!("Searching: {}", search_query);
        let uids = session.uid_search(&search_query).await?;

        // Filter out UIDs <= last_uid (IMAP UID ranges are inclusive and
        // the server may return the boundary UID)
        let new_uids: Vec<u32> = uids.into_iter().filter(|&u| u > last_uid).collect();

        if !new_uids.is_empty() {
            log::info!("Found {} new message(s)", new_uids.len());

            let uid_set: String = new_uids.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");

            let messages = session
                .uid_fetch(&uid_set, "BODY.PEEK[]")
                .await?
                .try_collect::<Vec<_>>()
                .await?;

            for msg in &messages {
                if let Some((msg_uid, processed)) =
                    process_message(msg, &rules, settings, browser).await
                {
                    // Move successfully processed emails to the target folder
                    if processed {
                        if let Some(ref folder) = settings.processed_folder {
                            match session.uid_mv(msg_uid.to_string(), folder).await {
                                Ok(_) => {
                                    log::info!("Moved UID {} to '{}'", msg_uid, folder);
                                }
                                Err(e) => {
                                    log::error!(
                                        "Failed to move UID {} to '{}': {}",
                                        msg_uid, folder, e
                                    );
                                }
                            }
                        }
                    }

                    // Always advance the UID watermark
                    if msg_uid > last_uid {
                        last_uid = msg_uid;
                        save_last_uid(&uid_state_path, last_uid);
                        log::debug!("Advanced last UID to {}", last_uid);
                    }
                }
            }
        }

        // Enter IDLE mode, wait up to 29 minutes (RFC 2177 recommendation)
        log::debug!("Entering IDLE mode...");
        let mut idle = session.idle();
        idle.init().await?;

        let (wait_future, _stop_source) = idle.wait_with_timeout(Duration::from_secs(29 * 60));

        match wait_future.await? {
            IdleResponse::Timeout => {
                log::debug!("IDLE timeout, re-issuing...");
            }
            IdleResponse::NewData(data) => {
                log::debug!("IDLE received new data: {:?}", data);
            }
            IdleResponse::ManualInterrupt => {
                log::debug!("IDLE manually interrupted");
            }
        }

        session = idle.done().await?;
    }
}

/// Replay mode: fetch and process specific messages, then exit.
/// Does not update the UID watermark. Does not move emails.
pub async fn run_replay(
    settings: &Settings,
    rules: &[Rule],
    browser: &crate::browser::BrowserSession,
    replay_arg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = connect(&settings.imap).await?;
    session.select(&settings.imap.mailbox).await?;

    log::info!("Replay mode: connecting to {}...", settings.imap.server);

    // Build the IMAP search/fetch based on the replay argument
    let uids: Vec<u32> = if let Some(n_str) = replay_arg.strip_prefix("last:") {
        // --replay last:10
        let n: u32 = n_str.parse().map_err(|_| format!("Invalid number: {}", n_str))?;
        log::info!("Replaying last {} message(s)", n);
        let all_uids = session.uid_search("ALL").await?;
        let mut sorted: Vec<u32> = all_uids.into_iter().collect();
        sorted.sort();
        sorted.into_iter().rev().take(n as usize).collect()
    } else if let Some(sender) = replay_arg.strip_prefix("from:") {
        // --replay from:receipts@openai.com
        let query = format!("FROM \"{}\"", sender);
        log::info!("Replaying messages from '{}'", sender);
        let result = session.uid_search(&query).await?;
        result.into_iter().collect()
    } else if let Some(subject) = replay_arg.strip_prefix("subject:") {
        // --replay subject:invoice
        let query = format!("SUBJECT \"{}\"", subject);
        log::info!("Replaying messages with subject matching '{}'", subject);
        let result = session.uid_search(&query).await?;
        result.into_iter().collect()
    } else {
        // --replay 121297 or --replay 121297,121298
        let uid_list: Result<Vec<u32>, _> = replay_arg
            .split(',')
            .map(|s| s.trim().parse::<u32>())
            .collect();
        match uid_list {
            Ok(uids) => {
                log::info!("Replaying {} specific UID(s)", uids.len());
                uids
            }
            Err(_) => {
                return Err(format!(
                    "Invalid --replay argument: '{}'\n\
                     Usage:\n  \
                       --replay 121297          Single UID\n  \
                       --replay 121297,121298   Multiple UIDs\n  \
                       --replay last:10         Last N messages\n  \
                       --replay from:user@x.com Messages from sender\n  \
                       --replay subject:invoice Messages with subject",
                    replay_arg
                )
                .into());
            }
        }
    };

    if uids.is_empty() {
        log::info!("No messages matched the replay criteria");
        return Ok(());
    }

    let uid_set: String = uids.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");
    log::info!("Fetching {} message(s): {}", uids.len(), uid_set);

    let messages = session
        .uid_fetch(&uid_set, "BODY.PEEK[]")
        .await?
        .try_collect::<Vec<_>>()
        .await?;

    let mut processed_count = 0;

    for msg in &messages {
        if let Some((msg_uid, processed)) =
            process_message(msg, rules, settings, browser).await
        {
            if processed {
                processed_count += 1;
                // In replay mode, do NOT move emails — this is a trial run
                log::info!("UID {}: processed successfully (not moved in replay mode)", msg_uid);
            }
        }
    }

    log::info!(
        "Replay complete: {} message(s) fetched, {} processed",
        messages.len(),
        processed_count
    );

    session.logout().await?;
    Ok(())
}
