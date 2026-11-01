mod browser;
mod config;
mod imap;
mod oauth;
mod rules;

use ini::Ini;
use std::time::Duration;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().collect();

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
        match imap::run_idle_loop(&settings, &rules, &browser_session).await {
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
