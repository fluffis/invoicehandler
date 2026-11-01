use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::browser::{
    DownloadProgressState, EventDownloadProgress, EventDownloadWillBegin,
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

use crate::config::Settings;

pub struct BrowserSession {
    browser: Browser,
    timeout: Duration,
    #[allow(dead_code)]
    download_dir: PathBuf,
}

impl BrowserSession {
    pub async fn launch(settings: &Settings) -> Result<Self, Box<dyn std::error::Error>> {
        let download_dir = std::env::temp_dir().join("invoicefetcher_downloads");
        log::debug!("Creating download directory: {}", download_dir.display());
        fs::create_dir_all(&download_dir).await?;

        let mut config_builder = BrowserConfig::builder().no_sandbox();

        if settings.browser_headless {
            log::debug!("Browser running in headless mode");
            config_builder = config_builder
                .new_headless_mode()
                .arg("--disable-gpu");
        } else {
            log::info!("Browser running in visible mode");
            config_builder = config_builder
                .with_head()
                .arg("--auto-open-devtools-for-tabs");
        }

        if let Some(ref binary) = settings.browser_binary {
            log::debug!("Using custom browser binary: {}", binary);
            config_builder = config_builder.chrome_executable(binary);
        }

        let config = config_builder
            .build()
            .map_err(|e| format!("Browser config error: {}", e))?;

        log::debug!("Launching browser...");
        let (browser, mut handler) = Browser::launch(config).await?;
        log::info!("Browser launched successfully");

        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                let _ = event;
            }
        });

        let download_params = SetDownloadBehaviorParams::builder()
            .behavior(SetDownloadBehaviorBehavior::AllowAndName)
            .download_path(download_dir.to_string_lossy().to_string())
            .events_enabled(true)
            .build()?;

        log::debug!(
            "Setting download behavior: allow, path={}",
            download_dir.display()
        );
        let page = browser.new_page("about:blank").await?;
        page.execute(download_params).await?;
        page.close().await?;

        Ok(BrowserSession {
            browser,
            timeout: Duration::from_secs(settings.browser_timeout_secs),
            download_dir,
        })
    }

    /// Navigate to a URL, find the element via CSS selector, click it to trigger a download,
    /// and wait for the file to be downloaded. Returns the path to the downloaded file.
    pub async fn fetch_download(
        &self,
        url: &str,
        download_selector: &str,
        output_directory: &Path,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        log::debug!("Navigating to: {}", url);
        let page = self.browser.new_page(url).await?;

        // Set download behavior on this page (CDP download events are per-target)
        let download_params = SetDownloadBehaviorParams::builder()
            .behavior(SetDownloadBehaviorBehavior::AllowAndName)
            .download_path(self.download_dir.to_string_lossy().to_string())
            .events_enabled(true)
            .build()?;
        page.execute(download_params).await?;
        log::debug!("Download behavior set on page");

        let mut download_begin_events = page.event_listener::<EventDownloadWillBegin>().await?;
        let mut download_events = page.event_listener::<EventDownloadProgress>().await?;

        log::debug!("Waiting for page navigation to complete...");
        page.wait_for_navigation().await?;
        log::debug!("Page loaded, waiting for selector: '{}'", download_selector);

        // Poll for the selector to appear in the DOM, retrying until timeout.
        // wait_for_navigation only waits for the load event — JS frameworks
        // often render content after that.
        let poll_interval = Duration::from_millis(500);
        let element = tokio::time::timeout(self.timeout, async {
            let count_js = format!(
                "document.querySelectorAll('{}').length",
                download_selector.replace('\'', "\\'")
            );
            loop {
                match page.evaluate(count_js.as_str()).await {
                    Ok(result) => {
                        let count = result.value().and_then(|v| v.as_u64()).unwrap_or(0);
                        if count > 0 {
                            if count > 1 {
                                log::debug!(
                                    "Selector '{}' matched {} elements, clicking first",
                                    download_selector,
                                    count
                                );
                            } else {
                                log::debug!("Selector '{}' matched 1 element", download_selector);
                            }
                            break;
                        }
                    }
                    Err(e) => {
                        log::debug!("Selector poll error: {}", e);
                    }
                }
                log::debug!("Selector '{}' not found yet, retrying...", download_selector);
                tokio::time::sleep(poll_interval).await;
            }
            page.find_element(download_selector).await
        })
        .await
        .map_err(|_| {
            log::error!("Timeout after {}s waiting for selector '{}'", self.timeout.as_secs(), download_selector);
            // Dump the HTML we see at timeout for debugging
            format!("Timeout waiting for selector '{}'", download_selector)
        })?
        .map_err(|e| format!("Failed to find element '{}': {}", download_selector, e))?;

        // Dump rendered HTML to disk for debugging (after selector found)
        if log::log_enabled!(log::Level::Debug) {
            match page.evaluate("document.documentElement.outerHTML").await {
                Ok(result) => {
                    if let Some(html) = result.value().and_then(|v| v.as_str()) {
                        let safe_name: String = url
                            .chars()
                            .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
                            .collect();
                        let dump_path = self.download_dir.join(format!("{}.html", &safe_name[..safe_name.len().min(120)]));
                        if let Err(e) = std::fs::write(&dump_path, html) {
                            log::debug!("Failed to write HTML dump: {}", e);
                        } else {
                            log::debug!("Page HTML saved to: {}", dump_path.display());
                        }
                    }
                }
                Err(e) => {
                    log::debug!("Failed to capture page HTML: {}", e);
                }
            }
        }

        // Log info about the element we're about to click
        match element.inner_text().await {
            Ok(Some(text)) => log::debug!("Element text: '{}'", text.chars().take(100).collect::<String>()),
            Ok(None) => log::debug!("Element has no inner text"),
            Err(_) => {}
        }
        match element.attribute("class").await {
            Ok(Some(class)) => log::debug!("Element class: '{}'", class),
            _ => {}
        }

        // Remember current pages so we can detect new tabs after clicking
        let mut pages_before = Vec::new();
        if let Ok(pages) = self.browser.pages().await {
            for p in &pages {
                if let Ok(Some(u)) = p.url().await {
                    pages_before.push(u);
                }
            }
        }

        log::info!("Clicking element '{}'", download_selector);
        element.click().await?;
        log::debug!("Click sent, waiting for download or new tab...");

        // Race between: CDP download event, or a new tab opening (e.g. window.open / target=_blank)
        let downloaded_path = tokio::time::timeout(self.timeout, async {
            let poll_interval = Duration::from_millis(500);
            let mut waited = Duration::ZERO;
            let mut suggested_filename: Option<String> = None;

            loop {
                // Drain any DownloadWillBegin events (carries the real filename)
                loop {
                    match tokio::time::timeout(Duration::from_millis(10), download_begin_events.next()).await {
                        Ok(Some(event)) => {
                            log::debug!(
                                "DownloadWillBegin: guid={}, suggested_filename='{}', url='{}'",
                                event.guid,
                                event.suggested_filename,
                                &event.url[..event.url.len().min(100)]
                            );
                            if !event.suggested_filename.is_empty() {
                                suggested_filename = Some(event.suggested_filename.clone());
                            }
                        }
                        _ => break,
                    }
                }
                // Check for CDP download events (non-blocking peek)
                match tokio::time::timeout(poll_interval, download_events.next()).await {
                    Ok(Some(event)) => {
                        match event.state {
                            DownloadProgressState::Completed => {
                                if let Some(ref path) = event.file_path {
                                    log::debug!("Download completed via CDP: {}", path);
                                    return Ok::<(PathBuf, Option<String>), Box<dyn std::error::Error>>((PathBuf::from(path), suggested_filename.clone()));
                                }
                                return Err("Download completed but no file path provided".into());
                            }
                            DownloadProgressState::Canceled => {
                                log::warn!("Download was canceled by the browser");
                                return Err("Download was canceled".into());
                            }
                            DownloadProgressState::InProgress => {
                                log::debug!(
                                    "Downloading: {:.0}/{:.0} bytes ({:.1}%)",
                                    event.received_bytes,
                                    event.total_bytes,
                                    if event.total_bytes > 0.0 {
                                        event.received_bytes / event.total_bytes * 100.0
                                    } else {
                                        0.0
                                    }
                                );
                                continue; // keep reading download events
                            }
                        }
                    }
                    Ok(None) => {
                        return Err("Download event stream ended without completion".into());
                    }
                    Err(_) => {
                        // No download event yet — check if a new tab was opened
                        waited += poll_interval;
                    }
                }

                // Check for new tabs (click may have opened a new page)
                if let Ok(pages) = self.browser.pages().await {
                    for new_page in &pages {
                        let page_url = new_page.url().await
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        if !page_url.is_empty()
                            && page_url != "about:blank"
                            && !pages_before.contains(&page_url)
                        {
                            log::info!("Click opened new tab: {}", page_url);

                            // If the URL looks like a direct file link, download it
                            if page_url.ends_with(".pdf")
                                || page_url.ends_with(".csv")
                                || page_url.ends_with(".zip")
                                || page_url.contains("/download")
                                || page_url.contains("content-disposition")
                            {
                                log::debug!("New tab URL looks like a download, fetching via HTTP...");
                                let response = reqwest::get(&page_url).await?;
                                let filename = extract_filename(&response, &page_url);
                                let bytes = response.bytes().await?;
                                let path = self.download_dir.join(&filename);
                                tokio::fs::write(&path, &bytes).await?;
                                new_page.clone().close().await.ok();
                                return Ok((path, Some(filename)));
                            }

                            // Otherwise wait for the new page to load and
                            // set up download behavior on it too
                            log::debug!("Waiting for new tab to finish loading...");
                            new_page.wait_for_navigation().await.ok();

                            // The new page might trigger a CDP download
                            // Give it a few seconds
                            log::debug!("Watching new tab for download events...");
                            continue;
                        }
                    }
                }

                if waited.as_secs() % 5 == 0 && waited.as_millis() > 0 {
                    log::debug!("Still waiting for download after {}s...", waited.as_secs());
                }
            }
        })
        .await
        .map_err(|_| {
            log::error!(
                "Timeout after {}s waiting for download to complete",
                self.timeout.as_secs()
            );
            "Timeout waiting for download to complete"
        })??;

        let (downloaded_path, suggested_filename) = downloaded_path;

        // Use the suggested filename from the DownloadWillBegin event if available,
        // otherwise fall back to the filename on disk (which may be a GUID)
        let filename = match suggested_filename {
            Some(ref name) if !name.is_empty() => {
                log::debug!("Using suggested filename: '{}'", name);
                name.clone()
            }
            _ => {
                let fallback = downloaded_path
                    .file_name()
                    .ok_or("Downloaded file has no name")?
                    .to_string_lossy()
                    .to_string();
                log::debug!("No suggested filename, using: '{}'", fallback);
                fallback
            }
        };
        let output_path = output_directory.join(&filename);
        log::debug!(
            "Moving {} -> {}",
            downloaded_path.display(),
            output_path.display()
        );
        fs::rename(&downloaded_path, &output_path).await?;

        page.close().await?;
        log::info!("File saved to: {}", output_path.display());

        Ok(output_path)
    }
}

/// Extract a filename from HTTP response headers or URL.
fn extract_filename(response: &reqwest::Response, url: &str) -> String {
    if let Some(cd) = response.headers().get("content-disposition") {
        if let Ok(cd_str) = cd.to_str() {
            if let Some(start) = cd_str.find("filename=") {
                let name = &cd_str[start + 9..];
                let name = name.trim_matches('"').trim_matches('\'');
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }

    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && s.contains('.'))
        .unwrap_or("download")
        .to_string()
}
