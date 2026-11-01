use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use tokio::fs;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REDIRECT_URI: &str = "http://127.0.0.1:8089";
const SCOPES: &str = "https://mail.google.com/";

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

/// Path to the stored OAuth token file, alongside the config file.
pub fn get_token_path() -> PathBuf {
    let config = crate::config::get_config_path();
    config.with_file_name(
        config
            .file_name()
            .map(|n| format!("{}.oauth_token", n.to_string_lossy()))
            .unwrap_or_else(|| "invoicefetcher.oauth_token".to_string()),
    )
}

/// Run the interactive OAuth2 authorization flow.
/// Opens a browser for the user to consent, listens on localhost for the callback,
/// exchanges the auth code for tokens, and saves them to disk.
pub async fn authorize(client_id: &str, client_secret: &str) -> Result<TokenData, Box<dyn std::error::Error>> {
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
        GOOGLE_AUTH_URL,
        urlencoded(client_id),
        urlencoded(REDIRECT_URI),
        urlencoded(SCOPES),
    );

    println!("Opening browser for Google authorization...");
    println!("If the browser doesn't open, visit this URL manually:");
    println!("{}", auth_url);

    if let Err(e) = open::that(&auth_url) {
        log::warn!("Could not open browser: {}", e);
    }

    // Listen for the OAuth callback
    let listener = TcpListener::bind("127.0.0.1:8089")?;
    println!("Waiting for authorization callback on {}...", REDIRECT_URI);

    let (mut stream, _) = listener.accept()?;
    let mut reader = std::io::BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    // Extract the authorization code from the request
    let code = request_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| {
            path.split('?')
                .nth(1)
                .and_then(|query| {
                    query.split('&').find_map(|param| {
                        let mut parts = param.splitn(2, '=');
                        match (parts.next(), parts.next()) {
                            (Some("code"), Some(code)) => Some(code.to_string()),
                            _ => None,
                        }
                    })
                })
        })
        .ok_or("No authorization code in callback")?;

    // Send a response to the browser
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h2>Authorization successful!</h2>\
        <p>You can close this tab and return to the terminal.</p></body></html>";
    let stream_ref: &mut std::net::TcpStream = &mut stream;
    stream_ref.write_all(response.as_bytes())?;
    stream_ref.flush()?;
    drop(stream);
    drop(listener);

    log::info!("Received authorization code, exchanging for tokens...");

    // Exchange auth code for tokens
    let client = reqwest::Client::new();
    let token_response: TokenResponse = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", REDIRECT_URI),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let token_data = TokenData {
        access_token: token_response.access_token,
        refresh_token: token_response
            .refresh_token
            .ok_or("No refresh token in response (was prompt=consent set?)")?,
        expires_at: now + token_response.expires_in,
    };

    save_token(&token_data).await?;
    println!("Authorization complete! Tokens saved.");

    Ok(token_data)
}

/// Get a valid access token, refreshing if expired.
pub async fn get_access_token(
    client_id: &str,
    client_secret: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let token_path = get_token_path();
    let token_data = load_token(&token_path).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Refresh if token expires within 60 seconds
    if now + 60 >= token_data.expires_at {
        log::debug!("Access token expired or expiring soon, refreshing...");
        let refreshed = refresh_token(client_id, client_secret, &token_data.refresh_token).await?;
        save_token(&refreshed).await?;
        Ok(refreshed.access_token)
    } else {
        log::debug!(
            "Access token valid for {}s",
            token_data.expires_at - now
        );
        Ok(token_data.access_token)
    }
}

async fn refresh_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<TokenData, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let token_response: TokenResponse = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    Ok(TokenData {
        access_token: token_response.access_token,
        // Google doesn't return a new refresh token on refresh, keep the old one
        refresh_token: token_response
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at: now + token_response.expires_in,
    })
}

async fn load_token(path: &Path) -> Result<TokenData, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path).await.map_err(|e| {
        format!(
            "Could not read token file '{}': {}. Run 'invoicefetcher --auth' first.",
            path.display(),
            e
        )
    })?;
    let token: TokenData = serde_json::from_str(&contents)?;
    Ok(token)
}

async fn save_token(token: &TokenData) -> Result<(), Box<dyn std::error::Error>> {
    let path = get_token_path();
    let json = serde_json::to_string_pretty(token)?;
    fs::write(&path, &json).await?;
    // Restrict permissions to owner only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    log::debug!("Token saved to {}", path.display());
    Ok(())
}

fn urlencoded(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                String::from(b as char)
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}
