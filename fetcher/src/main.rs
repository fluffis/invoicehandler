mod browser;
mod config;
mod imap;
mod oauth;
mod rules;

use ini::Ini;
use std::time::Duration;

/// A logger that writes to both syslog and stderr (for interactive replay mode).
struct DualLogger {
    syslog: syslog::BasicLogger,
}

impl log::Log for DualLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            // Write to syslog
            self.syslog.log(record);
            // Write to stderr
            eprintln!(
                "[{}] {}: {}",
                record.level(),
                record.module_path().unwrap_or(""),
                record.args()
            );
        }
    }

    fn flush(&self) {
        self.syslog.flush();
    }
}

fn init_logging(interactive: bool) {
    let formatter = syslog::Formatter3164 {
        facility: syslog::Facility::LOG_USER,
        hostname: None,
        process: "invoicefetcher".to_string(),
        pid: std::process::id(),
    };

    match syslog::unix(formatter) {
        Ok(logger) => {
            let level = if interactive {
                log::LevelFilter::Debug
            } else {
                log::LevelFilter::Info
            };

            if interactive {
                log::set_boxed_logger(Box::new(DualLogger {
                    syslog: syslog::BasicLogger::new(logger),
                }))
                .map(|()| log::set_max_level(level))
                .expect("Failed to set logger");
            } else {
                log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
                    .map(|()| log::set_max_level(level))
                    .expect("Failed to set logger");
            }
        }
        Err(e) => {
            eprintln!("Failed to connect to syslog: {}. Falling back to stderr.", e);
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let is_interactive = args.iter().any(|a| a == "--auth" || a == "--replay");
    init_logging(is_interactive);

    let config_path = config::get_config_path();
    log::info!("Loading config from: {}", config_path.display());

    let ini = Ini::load_from_file(&config_path).unwrap_or_else(|e| {
        eprintln!(
            "Failed to load config file '{}': {}",
            config_path.display(),
            e
        );
        std::process::exit(1);
    });

    let settings = config::load_settings(&ini);

    // Handle --auth flag for OAuth2 setup
    if args.iter().any(|a| a == "--auth") {
        match &settings.imap.auth {
            config::AuthMethod::OAuth2 {
                client_id,
                client_secret,
            } => {
                match oauth::authorize(client_id, client_secret).await {
                    Ok(_) => {
                        println!("OAuth2 authorization complete. You can now run invoicefetcher normally.");
                    }
                    Err(e) => {
                        eprintln!("OAuth2 authorization failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            config::AuthMethod::Password { .. } => {
                eprintln!("OAuth2 is not configured. Set auth_method = oauth2 in [imap] section first.");
                std::process::exit(1);
            }
        }
        return;
    }

    let rules = config::load_rules(&ini);

    if rules.is_empty() {
        eprintln!("No valid rules found in config. Exiting.");
        std::process::exit(1);
    }

    log::info!("Loaded {} rule(s)", rules.len());
    for rule in &rules {
        log::info!("  Rule '{}': sender={}", rule.name, rule.sender);
    }

    log::info!("Output directory: {}", settings.output_directory.display());

    // Handle --replay flag
    let replay_arg = args
        .iter()
        .position(|a| a == "--replay")
        .and_then(|i| args.get(i + 1))
        .cloned();

    if let Some(replay_value) = replay_arg {
        log::info!("Starting browser for replay...");
        let browser_session = match browser::BrowserSession::launch(&settings).await {
            Ok(b) => b,
            Err(e) => {
                log::error!("Failed to launch browser: {}", e);
                std::process::exit(1);
            }
        };

        match imap::run_replay(&settings, &rules, &browser_session, &replay_value).await {
            Ok(()) => {}
            Err(e) => {
                log::error!("Replay failed: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // Main reconnection loop
    loop {
        log::info!("Launching browser...");
        let browser_session = match browser::BrowserSession::launch(&settings).await {
            Ok(b) => b,
            Err(e) => {
                log::error!(
                    "Failed to launch browser: {}. Retrying in {}s...",
                    e,
                    settings.reconnect_delay_secs
                );
                tokio::time::sleep(Duration::from_secs(settings.reconnect_delay_secs)).await;
                continue;
            }
        };

        log::info!("Connecting to IMAP server...");
        // Rules are reloaded from config inside run_idle_loop on each cycle,
        // but we pass the initially loaded rules for the first iteration.
        let current_rules = config::load_rules(&ini);
        match imap::run_idle_loop(&settings, current_rules, &browser_session).await {
            Ok(()) => unreachable!(),
            Err(e) => {
                log::error!(
                    "IMAP error: {}. Reconnecting in {}s...",
                    e,
                    settings.reconnect_delay_secs
                );
                tokio::time::sleep(Duration::from_secs(settings.reconnect_delay_secs)).await;
            }
        }
    }
}
