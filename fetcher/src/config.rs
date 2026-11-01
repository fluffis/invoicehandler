use ini::Ini;
use regex::Regex;
use std::path::PathBuf;

pub enum AuthMethod {
    Password { password: String },
    OAuth2 { client_id: String, client_secret: String },
}

pub struct ImapSettings {
    pub server: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub mailbox: String,
}

pub struct Settings {
    pub imap: ImapSettings,
    pub output_directory: PathBuf,
    pub reconnect_delay_secs: u64,
    pub browser_timeout_secs: u64,
    pub browser_binary: Option<String>,
    pub browser_headless: bool,
    /// IMAP folder to move successfully processed emails to. If unset, emails are left in place.
    pub processed_folder: Option<String>,
}

pub enum RuleAction {
    /// Open a link from the email body in a browser and click a download button.
    BrowserDownload {
        link_pattern: Regex,
        download_selector: String,
    },
    /// Save an email attachment matching the filename pattern.
    SaveAttachment {
        attachment_pattern: Regex,
    },
}

pub struct Rule {
    pub name: String,
    pub sender: String,
    pub subject_pattern: Option<Regex>,
    pub action: RuleAction,
}

pub fn get_config_path() -> PathBuf {
    if cfg!(target_os = "linux") {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join(".invoicefetcher")
    } else if cfg!(target_os = "macos") {
        dirs::config_dir()
            .expect("Could not determine config directory")
            .join("invoicefetcher")
            .join("config.ini")
    } else if cfg!(target_os = "windows") {
        dirs::config_dir()
            .expect("Could not determine config directory")
            .join("invoicefetcher")
            .join("config.ini")
    } else {
        panic!("Unsupported platform");
    }
}

/// Path to the file that stores the last processed UID.
/// Stored alongside the config file.
pub fn get_uid_state_path() -> PathBuf {
    let config = get_config_path();
    config.with_file_name(
        config
            .file_name()
            .map(|n| format!("{}.uid", n.to_string_lossy()))
            .unwrap_or_else(|| "invoicefetcher.uid".to_string()),
    )
}

pub fn load_settings(ini: &Ini) -> Settings {
    let imap_section = ini.section(Some("imap")).expect("Missing [imap] section");
    let settings_section = ini.section(Some("settings")).expect("Missing [settings] section");

    let auth_method = imap_section.get("auth_method").unwrap_or("password");
    let auth = match auth_method {
        "oauth2" => {
            let client_id = imap_section
                .get("client_id")
                .expect("Missing imap.client_id for oauth2 auth")
                .to_string();
            let client_secret = imap_section
                .get("client_secret")
                .expect("Missing imap.client_secret for oauth2 auth")
                .to_string();
            AuthMethod::OAuth2 {
                client_id,
                client_secret,
            }
        }
        "password" | _ => {
            let password = imap_section
                .get("password")
                .expect("Missing imap.password")
                .to_string();
            AuthMethod::Password { password }
        }
    };

    let imap = ImapSettings {
        server: imap_section
            .get("server")
            .expect("Missing imap.server")
            .to_string(),
        port: imap_section
            .get("port")
            .unwrap_or("993")
            .parse()
            .expect("Invalid imap.port"),
        username: imap_section
            .get("username")
            .expect("Missing imap.username")
            .to_string(),
        auth,
        mailbox: imap_section
            .get("mailbox")
            .unwrap_or("INBOX")
            .to_string(),
    };

    Settings {
        imap,
        output_directory: PathBuf::from(
            settings_section
                .get("output_directory")
                .expect("Missing settings.output_directory"),
        ),
        reconnect_delay_secs: settings_section
            .get("reconnect_delay_secs")
            .unwrap_or("30")
            .parse()
            .unwrap_or(30),
        browser_timeout_secs: settings_section
            .get("browser_timeout_secs")
            .unwrap_or("30")
            .parse()
            .unwrap_or(30),
        browser_binary: settings_section.get("browser_binary").map(|s| s.to_string()),
        browser_headless: settings_section
            .get("browser_headless")
            .unwrap_or("true")
            .eq_ignore_ascii_case("true"),
        processed_folder: settings_section
            .get("processed_folder")
            .map(|s| s.to_string()),
    }
}

pub fn load_rules(ini: &Ini) -> Vec<Rule> {
    let mut rules = Vec::new();

    for (section_name, properties) in ini.iter() {
        let section_name = match section_name {
            Some(name) if name.starts_with("rule.") => name,
            _ => continue,
        };
        let rule_name = section_name.strip_prefix("rule.").unwrap().to_string();

        let sender = match properties.get("sender") {
            Some(s) => s.to_string(),
            None => {
                eprintln!("Rule '{}': missing sender, skipping", rule_name);
                continue;
            }
        };

        let subject_pattern = match properties.get("subject_pattern") {
            Some(pat) => match Regex::new(pat) {
                Ok(re) => Some(re),
                Err(e) => {
                    eprintln!(
                        "Rule '{}': invalid subject_pattern '{}': {}, skipping",
                        rule_name, pat, e
                    );
                    continue;
                }
            },
            None => None,
        };

        // Determine rule type: attachment_pattern OR (link_pattern + download_selector)
        let has_attachment = properties.get("attachment_pattern").is_some();
        let has_link = properties.get("link_pattern").is_some();

        let action = if has_attachment {
            let pat = properties.get("attachment_pattern").unwrap();
            match Regex::new(pat) {
                Ok(re) => RuleAction::SaveAttachment {
                    attachment_pattern: re,
                },
                Err(e) => {
                    eprintln!(
                        "Rule '{}': invalid attachment_pattern '{}': {}, skipping",
                        rule_name, pat, e
                    );
                    continue;
                }
            }
        } else if has_link {
            let link_pattern = match Regex::new(properties.get("link_pattern").unwrap()) {
                Ok(re) => re,
                Err(e) => {
                    eprintln!(
                        "Rule '{}': invalid link_pattern '{}': {}, skipping",
                        rule_name,
                        properties.get("link_pattern").unwrap(),
                        e
                    );
                    continue;
                }
            };

            let download_selector = match properties.get("download_selector") {
                Some(s) => s.to_string(),
                None => {
                    eprintln!("Rule '{}': missing download_selector, skipping", rule_name);
                    continue;
                }
            };

            RuleAction::BrowserDownload {
                link_pattern,
                download_selector,
            }
        } else {
            eprintln!(
                "Rule '{}': must have either attachment_pattern or link_pattern, skipping",
                rule_name
            );
            continue;
        };

        rules.push(Rule {
            name: rule_name,
            sender,
            subject_pattern,
            action,
        });
    }

    rules
}
