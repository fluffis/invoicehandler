use crate::config::{Rule, RuleAction};

pub struct Email {
    pub from: String,
    pub subject: String,
    pub body_html: String,
    pub attachments: Vec<Attachment>,
}

pub struct Attachment {
    pub filename: String,
    pub data: Vec<u8>,
}

pub enum MatchResult {
    /// Open this URL in a browser and click the selector to download.
    BrowserDownload {
        url: String,
        download_selector: String,
    },
    /// Save this attachment directly.
    SaveAttachment {
        filename: String,
        data: Vec<u8>,
    },
}

/// Find the first matching rule for an email and return the action to take.
pub fn match_email<'a>(email: &Email, rules: &'a [Rule]) -> Option<(&'a Rule, MatchResult)> {
    for rule in rules {
        log::debug!(
            "Rule '{}': checking sender='{}' against from='{}'",
            rule.name,
            rule.sender,
            email.from
        );

        // Case-insensitive substring match on sender
        if !email
            .from
            .to_lowercase()
            .contains(&rule.sender.to_lowercase())
        {
            log::debug!("Rule '{}': sender did not match", rule.name);
            continue;
        }
        log::debug!("Rule '{}': sender matched", rule.name);

        // Optional subject regex match
        if let Some(ref subject_re) = rule.subject_pattern {
            if !subject_re.is_match(&email.subject) {
                log::debug!(
                    "Rule '{}': subject '{}' did not match pattern '{}'",
                    rule.name,
                    email.subject,
                    subject_re.as_str()
                );
                continue;
            }
            log::debug!("Rule '{}': subject matched", rule.name);
        }

        match &rule.action {
            RuleAction::BrowserDownload {
                link_pattern,
                download_selector,
            } => {
                log::debug!(
                    "Rule '{}': searching body ({} bytes) with link_pattern '{}'",
                    rule.name,
                    email.body_html.len(),
                    link_pattern.as_str()
                );
                if let Some(captures) = link_pattern.captures(&email.body_html) {
                    if let Some(url) = captures.get(1) {
                        log::debug!(
                            "Rule '{}': link_pattern matched, URL: {}",
                            rule.name,
                            url.as_str()
                        );
                        return Some((
                            rule,
                            MatchResult::BrowserDownload {
                                url: url.as_str().to_string(),
                                download_selector: download_selector.clone(),
                            },
                        ));
                    }
                    log::debug!(
                        "Rule '{}': link_pattern matched but no capture group 1",
                        rule.name
                    );
                } else {
                    log::debug!("Rule '{}': link_pattern did not match body", rule.name);
                }
            }
            RuleAction::SaveAttachment { attachment_pattern } => {
                log::debug!(
                    "Rule '{}': checking {} attachment(s) with pattern '{}'",
                    rule.name,
                    email.attachments.len(),
                    attachment_pattern.as_str()
                );
                for att in &email.attachments {
                    log::debug!(
                        "Rule '{}': checking attachment '{}'",
                        rule.name,
                        att.filename
                    );
                    if attachment_pattern.is_match(&att.filename) {
                        log::debug!(
                            "Rule '{}': attachment '{}' matched",
                            rule.name,
                            att.filename
                        );
                        return Some((
                            rule,
                            MatchResult::SaveAttachment {
                                filename: att.filename.clone(),
                                data: att.data.clone(),
                            },
                        ));
                    }
                }
                log::debug!(
                    "Rule '{}': no attachment matched pattern",
                    rule.name
                );
            }
        }
    }

    None
}
