use log::{debug, error, info, warn};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify_rust::{Notification, Urgency};
use regex::Regex;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

struct Settings {
    watch_directory: PathBuf,
    max_lock_retries: u32,
    lock_retry_delay_ms: u64,
}

fn load_settings(config_path: &Path) -> Result<Settings, String> {
    let ini = ini::Ini::load_from_file(config_path)
        .map_err(|e| format!("Failed to load config.ini: {}", e))?;

    let section = ini
        .section(Some("settings"))
        .ok_or("Missing [settings] section in config.ini")?;

    let watch_directory = section
        .get("watch_directory")
        .ok_or("Missing 'watch_directory' in [settings]")?;

    let max_lock_retries: u32 = section
        .get("max_lock_retries")
        .unwrap_or("30")
        .parse()
        .map_err(|e| format!("Invalid max_lock_retries: {}", e))?;

    let lock_retry_delay_ms: u64 = section
        .get("lock_retry_delay_ms")
        .unwrap_or("1000")
        .parse()
        .map_err(|e| format!("Invalid lock_retry_delay_ms: {}", e))?;

    Ok(Settings {
        watch_directory: PathBuf::from(watch_directory),
        max_lock_retries,
        lock_retry_delay_ms,
    })
}

fn load_rules(config_path: &Path) -> Result<Vec<(Regex, String)>, String> {
    let ini = ini::Ini::load_from_file(config_path)
        .map_err(|e| format!("Failed to load config.ini: {}", e))?;

    let mut rules = Vec::new();

    if let Some(section) = ini.section(Some("translations")) {
        for (pattern, replacement) in section.iter() {
            match Regex::new(pattern) {
                Ok(regex) => {
                    rules.push((regex, replacement.to_string()));
                    info!("Loaded rule: {} -> {}", pattern, replacement);
                }
                Err(e) => {
                    warn!("Skipping invalid regex pattern '{}': {}", pattern, e);
                }
            }
        }
    }

    Ok(rules)
}

fn wait_for_file_unlock(file_path: &Path, settings: &Settings) -> bool {
    for attempt in 1..=settings.max_lock_retries {
        match OpenOptions::new().read(true).write(true).open(file_path) {
            Ok(_file) => {
                return true;
            }
            Err(e) => {
                if attempt < settings.max_lock_retries {
                    debug!(
                        "File '{}' is locked (attempt {}/{}): {}. Retrying...",
                        file_path.display(),
                        attempt,
                        settings.max_lock_retries,
                        e
                    );
                    thread::sleep(Duration::from_millis(settings.lock_retry_delay_ms));
                } else {
                    error!(
                        "File '{}' remained locked after {} attempts. Skipping.",
                        file_path.display(),
                        settings.max_lock_retries
                    );
                    return false;
                }
            }
        }
    }
    false
}

fn apply_rename(file_path: &Path, rules: &[(Regex, String)], settings: &Settings) {
    if !file_path.exists() {
        return;
    }

    let filename = match file_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return,
    };

    debug!("Extracted filename: {}", filename);

    if !wait_for_file_unlock(file_path, settings) {
        return;
    }

    for (regex, replacement) in rules {
        if regex.is_match(filename) {
            let new_filename = regex.replace(filename, replacement.as_str()).to_string();

            if new_filename != filename {
                let new_path = file_path.with_file_name(&new_filename);

                match fs::rename(file_path, &new_path) {
                    Ok(()) => {
                        info!("Renamed: {} -> {}", filename, new_filename);
                        let _ = Notification::new()
                            .summary("File Renamed")
                            .body(&format!("{} -> {}", filename, new_filename))
                            .urgency(Urgency::Normal)
                            .show();
                    }
                    Err(e) => {
                        error!(
                            "Failed to rename '{}' to '{}': {}",
                            filename, new_filename, e
                        );
                        let _ = Notification::new()
                            .summary("Rename Failed")
                            .body(&format!(
                                "Failed to rename '{}' to '{}': {}",
                                filename, new_filename, e
                            ))
                            .urgency(Urgency::Critical)
                            .show();
                    }
                }
            }
            return;
        }
    }

    debug!("No matching rule for: {}", filename);
}

fn get_config_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        dirs::home_dir()
            .expect("Failed to get home directory")
            .join(".invoicehandler")
    }

    #[cfg(target_os = "macos")]
    {
        dirs::config_dir()
            .expect("Failed to get config directory")
            .join("invoicehandler")
            .join("config.ini")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .expect("Failed to get config directory")
            .join("invoicehandler")
            .join("config.ini")
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir()
            .expect("Failed to get config directory")
            .join("invoicehandler")
            .join("config.ini")
    }
}

fn init_logging() {
    let formatter = syslog::Formatter3164 {
        facility: syslog::Facility::LOG_USER,
        hostname: None,
        process: "invoicehandler".to_string(),
        pid: std::process::id(),
    };

    match syslog::unix(formatter) {
        Ok(logger) => {
            log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
                .map(|()| log::set_max_level(log::LevelFilter::Info))
                .expect("Failed to set logger");
        }
        Err(e) => {
            eprintln!("Failed to connect to syslog: {}. Falling back to stderr.", e);
        }
    }
}

fn main() {
    init_logging();

    let config_path = get_config_path();
    if !config_path.exists() {
        error!("Config file not found at {:?}", config_path);
        std::process::exit(1);
    }

    let settings = match load_settings(&config_path) {
        Ok(s) => s,
        Err(e) => {
            error!("Error loading settings: {}", e);
            std::process::exit(1);
        }
    };

    if !settings.watch_directory.is_dir() {
        error!(
            "'{}' is not a valid directory",
            settings.watch_directory.display()
        );
        std::process::exit(1);
    }

    let mut rules = match load_rules(&config_path) {
        Ok(r) => r,
        Err(e) => {
            error!("Error loading rules: {}", e);
            std::process::exit(1);
        }
    };

    if rules.is_empty() {
        warn!("No valid translation rules loaded");
    }

    info!("Watching directory: {:?}", settings.watch_directory);
    info!("Watching config: {:?}", config_path);
    info!("Loaded {} translation rules", rules.len());

    let (tx, rx) = channel();

    let tx_clone = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            if let Ok(event) = result {
                let _ = tx_clone.send(event);
            }
        },
        Config::default(),
    )
    .expect("Failed to create file watcher");

    watcher
        .watch(&settings.watch_directory, RecursiveMode::NonRecursive)
        .expect("Failed to watch directory");

    watcher
        .watch(&config_path, RecursiveMode::NonRecursive)
        .expect("Failed to watch config file");

    info!("File watcher started");

    for event in rx {
        debug!("Event received: {:?}", event.kind);
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in &event.paths {
                    if path == &config_path {
                        info!("Config file changed, reloading rules...");
                        match load_rules(&config_path) {
                            Ok(new_rules) => {
                                rules = new_rules;
                                info!("Reloaded {} translation rules", rules.len());
                            }
                            Err(e) => {
                                error!("Failed to reload config: {}. Keeping old rules.", e);
                            }
                        }
                    } else {
                        debug!("Found file at {:?}", &path);
                        apply_rename(path, &rules, &settings);
                    }
                }
            }
            _ => {}
        }
    }
}
