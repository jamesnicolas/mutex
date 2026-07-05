//! Loopback OAuth 2.0 callback server and shared HTML response page.
//!
//! Used by Mutex's OAuth-based sign-in flows (e.g. MCP servers, ChatGPT
//! Subscription) to receive the authorization code redirect from the user's
//! browser. The HTML response page rendered to the browser is kept alongside
//! the server so all OAuth callback presentation lives in one place.

/// Generate a styled HTML page for OAuth callback responses.
///
/// Returns a complete HTML document (no HTTP headers) with a centered card
/// layout styled to match Mutex's dark theme. The `title` is rendered as a
/// heading and `message` as body text below it.
///
/// When `is_error` is true, a red X icon is shown instead of the green
/// checkmark.
pub fn oauth_callback_page(title: &str, message: &str, is_error: bool) -> String {
    let title = html_escape(title);
    let message = html_escape(message);
    let (icon_bg, icon_svg) = if is_error {
        (
            "#f38ba8",
            r#"<svg viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>"#,
        )
    } else {
        (
            "#a6e3a1",
            r#"<svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg>"#,
        )
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — Mutex</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
    background: #1e1e2e;
    color: #cdd6f4;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100vh;
    padding: 1rem;
  }}
  .card {{
    background: #313244;
    border-radius: 12px;
    padding: 2.5rem;
    max-width: 420px;
    width: 100%;
    text-align: center;
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.3);
  }}
  .icon {{
    width: 48px;
    height: 48px;
    margin: 0 auto 1.5rem;
    background: {icon_bg};
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
  }}
  .icon svg {{
    width: 24px;
    height: 24px;
    stroke: #1e1e2e;
    stroke-width: 3;
    fill: none;
  }}
  h1 {{
    font-size: 1.25rem;
    font-weight: 600;
    margin-bottom: 0.75rem;
    color: #cdd6f4;
  }}
  p {{
    font-size: 0.925rem;
    line-height: 1.5;
    color: #a6adc8;
  }}
  .brand {{
    margin-top: 1.5rem;
    font-size: 0.8rem;
    color: #585b70;
    letter-spacing: 0.05em;
  }}
</style>
</head>
<body>
<div class="card">
  <div class="icon">
    {icon_svg}
  </div>
  <h1>{title}</h1>
  <p>{message}</p>
  <div class="brand">Mutex</div>
</div>
</body>
</html>"#,
        title = title,
        message = message,
        icon_bg = icon_bg,
        icon_svg = icon_svg,
    )
}

fn html_escape(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#x27;"),
            _ => output.push(ch),
        }
    }
    output
}

pub const CODEX_CREDENTIALS_KEY: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

// The OAuth client registered for `CLIENT_ID` (the Codex CLI's client) only allows
// `http://localhost:1455/auth/callback` and `http://localhost:1457/auth/callback`
// as redirect URIs; using anything else (different host, port, or path) causes
// auth.openai.com to reject the authorize request with a generic `unknown_error`
// before redirecting back. Keep these in sync with the Codex CLI's redirect URI
// allow-list (see codex-rs/login/src/server.rs in openai/codex).
pub const CODEX_CALLBACK_HOST: &str = "localhost";
pub const CODEX_CALLBACK_PORT: u16 = 1455;
pub const CODEX_CALLBACK_FALLBACK_PORT: u16 = 1457;
pub const CODEX_CALLBACK_PATH: &str = "/auth/callback";
pub const CODEX_CALLBACK_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

#[derive(Clone, Debug)]
pub struct CodexOauthFlow {
    pub authorize_url: String,
    pub state: String,
    pub verifier: String,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: u64,
    pub account_id: Option<String>,
    pub email: Option<String>,
}

impl CodexCredentials {
    pub fn expires_within(&self, buffer_ms: u64) -> bool {
        now_ms() + buffer_ms >= self.expires_at_ms
    }
}

#[derive(Debug)]
pub enum CodexTokenRefreshError {
    Fatal(anyhow::Error),
    Transient(anyhow::Error),
}

impl std::fmt::Display for CodexTokenRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fatal(error) => write!(f, "{error}"),
            Self::Transient(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CodexTokenRefreshError {}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    email: Option<String>,
}

pub fn build_codex_oauth_flow(redirect_uri: &str) -> anyhow::Result<CodexOauthFlow> {
    use anyhow::Context as _;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::RngCore as _;
    use sha2::{Digest, Sha256};

    let mut verifier_bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    let mut state_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut state_bytes);
    let state: String = state_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let mut authorize_url =
        url::Url::parse(OPENAI_AUTHORIZE_URL).context("parse OpenAI authorize URL")?;
    authorize_url
        .query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("response_type", "code")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("state", &state)
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "zed");

    Ok(CodexOauthFlow {
        authorize_url: authorize_url.to_string(),
        state,
        verifier,
    })
}

pub async fn exchange_codex_code(
    client: &std::sync::Arc<dyn http_client::HttpClient>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<CodexCredentials> {
    use anyhow::Context as _;

    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_verifier", verifier)
        .finish();

    let tokens = send_token_request(client, body)
        .await
        .context("exchange Codex authorization code")?;
    Ok(credentials_from_token_response(tokens))
}

pub async fn refresh_codex_token(
    client: &std::sync::Arc<dyn http_client::HttpClient>,
    refresh_token: &str,
) -> Result<CodexCredentials, CodexTokenRefreshError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("refresh_token", refresh_token)
        .finish();

    let request = http_client::Request::builder()
        .method(http_client::Method::POST)
        .uri(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(http_client::AsyncBody::from(body))
        .map_err(|error| CodexTokenRefreshError::Transient(error.into()))?;

    let mut response = client
        .send(request)
        .await
        .map_err(CodexTokenRefreshError::Transient)?;
    let status = response.status();
    let mut body = String::new();
    smol::io::AsyncReadExt::read_to_string(response.body_mut(), &mut body)
        .await
        .map_err(|error| CodexTokenRefreshError::Transient(error.into()))?;

    if !status.is_success() {
        let error = anyhow::anyhow!("Token refresh failed (HTTP {}): {body}", status);
        if status == http_client::StatusCode::BAD_REQUEST
            || status == http_client::StatusCode::UNAUTHORIZED
            || status == http_client::StatusCode::FORBIDDEN
        {
            return Err(CodexTokenRefreshError::Fatal(error));
        }
        return Err(CodexTokenRefreshError::Transient(error));
    }

    let tokens: TokenResponse = serde_json::from_str(&body)
        .map_err(|error| CodexTokenRefreshError::Transient(error.into()))?;
    Ok(credentials_from_token_response(tokens))
}

async fn send_token_request(
    client: &std::sync::Arc<dyn http_client::HttpClient>,
    body: String,
) -> anyhow::Result<TokenResponse> {
    use anyhow::Context as _;

    let request = http_client::Request::builder()
        .method(http_client::Method::POST)
        .uri(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(http_client::AsyncBody::from(body))?;

    let mut response = client.send(request).await?;
    let mut body = String::new();
    smol::io::AsyncReadExt::read_to_string(response.body_mut(), &mut body).await?;

    if !response.status().is_success() {
        anyhow::bail!("Token exchange failed (HTTP {}): {body}", response.status());
    }

    serde_json::from_str::<TokenResponse>(&body).context("parse Codex token response")
}

fn credentials_from_token_response(tokens: TokenResponse) -> CodexCredentials {
    let jwt = tokens
        .id_token
        .as_deref()
        .unwrap_or(tokens.access_token.as_str());
    let claims = extract_jwt_claims(jwt);

    CodexCredentials {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at_ms: now_ms() + tokens.expires_in * 1000,
        account_id: claims.account_id,
        email: claims.email.or(tokens.email),
    }
}

struct JwtClaims {
    account_id: Option<String>,
    email: Option<String>,
}

fn extract_jwt_claims(jwt: &str) -> JwtClaims {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let Some(payload_b64) = jwt.split('.').nth(1) else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };
    let Ok(payload) = URL_SAFE_NO_PAD.decode(payload_b64) else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };

    let account_id = claims
        .get("chatgpt_account_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(|value| value.as_array())
                .and_then(|array| array.first())
                .and_then(|organization| organization.get("id"))
                .and_then(|value| value.as_str())
        })
        .map(ToOwned::to_owned);

    let email = claims
        .get("email")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);

    JwtClaims { account_id, email }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_else(|error| {
            log::error!("System clock is before UNIX epoch: {error}");
            0
        })
}

#[cfg(not(target_family = "wasm"))]
mod server {
    use super::oauth_callback_page;
    use anyhow::{Context as _, Result, anyhow};
    use std::str::FromStr;
    use std::time::Duration;
    use url::Url;

    /// Parsed OAuth callback parameters from the authorization server redirect.
    pub struct OAuthCallbackParams {
        pub code: String,
        pub state: String,
    }

    /// Configuration for the loopback OAuth callback server.
    ///
    /// OAuth servers compare `redirect_uri` against a per-client allow-list using
    /// exact string matching (RFC 6749 §3.1.2), so the `host`, `preferred_port`,
    /// and `path` here must match what's registered for the OAuth client_id.
    #[derive(Clone, Copy)]
    pub struct OAuthCallbackServerConfig {
        /// Host portion of the redirect URI (typically `127.0.0.1` or `localhost`).
        pub host: &'static str,
        /// Preferred port. Use `0` for an OS-assigned ephemeral port.
        pub preferred_port: u16,
        /// Optional fallback port if `preferred_port` is unavailable. Only used
        /// when `preferred_port` is non-zero.
        pub fallback_port: Option<u16>,
        /// Callback path on the redirect URI (e.g. `/callback`, `/auth/callback`).
        pub path: &'static str,
    }

    impl Default for OAuthCallbackServerConfig {
        fn default() -> Self {
            Self {
                host: "127.0.0.1",
                preferred_port: 0,
                fallback_port: None,
                path: "/callback",
            }
        }
    }

    impl OAuthCallbackParams {
        /// Parse the query string from a callback URL like
        /// `http://127.0.0.1:<port>/callback?code=...&state=...`.
        pub fn parse_query(query: &str) -> Result<Self> {
            let mut code: Option<String> = None;
            let mut state: Option<String> = None;
            let mut error: Option<String> = None;
            let mut error_description: Option<String> = None;

            for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
                match key.as_ref() {
                    "code" => {
                        if !value.is_empty() {
                            code = Some(value.into_owned());
                        }
                    }
                    "state" => {
                        if !value.is_empty() {
                            state = Some(value.into_owned());
                        }
                    }
                    "error" => {
                        if !value.is_empty() {
                            error = Some(value.into_owned());
                        }
                    }
                    "error_description" => {
                        if !value.is_empty() {
                            error_description = Some(value.into_owned());
                        }
                    }
                    _ => {}
                }
            }

            if let Some(error_code) = error {
                anyhow::bail!(
                    "OAuth authorization failed: {} ({})",
                    error_code,
                    error_description.as_deref().unwrap_or("no description")
                );
            }

            let code = code.ok_or_else(|| anyhow!("missing 'code' parameter in OAuth callback"))?;
            let state =
                state.ok_or_else(|| anyhow!("missing 'state' parameter in OAuth callback"))?;

            Ok(Self { code, state })
        }
    }

    /// How long to wait for the browser to complete the OAuth flow before giving
    /// up and releasing the loopback port.
    const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(2 * 60);

    /// Start a loopback HTTP server to receive the OAuth authorization callback.
    ///
    /// Binds to an ephemeral loopback port. Returns `(redirect_uri, callback_future)`.
    /// The caller should use the redirect URI in the authorization request, open
    /// the browser, then await the future to receive the callback.
    pub fn start_oauth_callback_server() -> Result<(
        String,
        futures::channel::oneshot::Receiver<Result<OAuthCallbackParams>>,
    )> {
        start_oauth_callback_server_with_config(OAuthCallbackServerConfig::default())
    }

    /// Start a loopback HTTP server with custom host/port/path.
    ///
    /// Use this when the OAuth client requires a specific redirect URI that the
    /// default ephemeral-port `http://127.0.0.1:<port>/callback` doesn't match.
    pub fn start_oauth_callback_server_with_config(
        config: OAuthCallbackServerConfig,
    ) -> Result<(
        String,
        futures::channel::oneshot::Receiver<Result<OAuthCallbackParams>>,
    )> {
        let server = bind_callback_server(&config)?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| anyhow!("server not bound to a TCP address"))?
            .port();

        let redirect_uri = format!("http://{}:{}{}", config.host, port, config.path);
        let expected_path = config.path;

        let (tx, rx) = futures::channel::oneshot::channel();

        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + OAUTH_CALLBACK_TIMEOUT;

            loop {
                if tx.is_canceled() {
                    return;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return;
                }

                let timeout = remaining.min(Duration::from_millis(500));
                let Some(request) = (match server.recv_timeout(timeout) {
                    Ok(req) => req,
                    Err(_) => {
                        let _ = tx.send(Err(anyhow!("OAuth callback server I/O error")));
                        return;
                    }
                }) else {
                    continue;
                };

                let raw_url = request.url().to_string();
                let raw_path = raw_url.split('?').next().unwrap_or(&raw_url);
                if raw_path == CANCEL_PATH {
                    let response = tiny_http::Response::from_string("Cancelled")
                        .with_status_code(200)
                        .with_header(
                            tiny_http::Header::from_str("Content-Type: text/plain")
                                .expect("failed to construct response header"),
                        )
                        .with_header(
                            tiny_http::Header::from_str("Connection: close")
                                .expect("failed to construct response header"),
                        );
                    if let Err(err) = request.respond(response) {
                        log::error!("Failed to send OAuth cancel response: {}", err);
                    }
                    let _ = tx.send(Err(anyhow!(
                        "OAuth callback server was cancelled by another sign-in attempt"
                    )));
                    return;
                }

                let result = handle_oauth_callback_request(&request, expected_path);

                let (status_code, body) = match &result {
                    Ok(_) => (
                        200,
                        oauth_callback_page(
                            "Authorization Successful",
                            "You can close this tab and return to Mutex.",
                            false,
                        ),
                    ),
                    Err(err) => {
                        log::error!("OAuth callback error: {}", err);
                        (
                            400,
                            oauth_callback_page(
                                "Authorization Failed",
                                "Something went wrong. Please try again from Mutex.",
                                true,
                            ),
                        )
                    }
                };

                let response = tiny_http::Response::from_string(body)
                    .with_status_code(status_code)
                    .with_header(
                        tiny_http::Header::from_str("Content-Type: text/html")
                            .expect("failed to construct response header"),
                    )
                    .with_header(
                        tiny_http::Header::from_str("Keep-Alive: timeout=0,max=0")
                            .expect("failed to construct response header"),
                    );
                if let Err(err) = request.respond(response) {
                    log::error!("Failed to send OAuth callback response: {}", err);
                }

                let _ = tx.send(result);
                return;
            }
        });

        Ok((redirect_uri, rx))
    }

    fn handle_oauth_callback_request(
        request: &tiny_http::Request,
        expected_path: &str,
    ) -> Result<OAuthCallbackParams> {
        let url = Url::parse(&format!("http://localhost{}", request.url()))
            .context("malformed callback request URL")?;

        if url.path() != expected_path {
            anyhow::bail!("unexpected path in OAuth callback: {}", url.path());
        }

        let query = url
            .query()
            .ok_or_else(|| anyhow!("OAuth callback has no query string"))?;
        OAuthCallbackParams::parse_query(query)
    }

    /// Callback path reserved for evicting a previously-running OAuth callback
    /// server bound to the same port. Always handled, regardless of `config.path`.
    const CANCEL_PATH: &str = "/cancel";

    const BIND_MAX_ATTEMPTS: u32 = 10;
    const BIND_RETRY_DELAY: Duration = Duration::from_millis(200);
    const CANCEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn bind_callback_server(config: &OAuthCallbackServerConfig) -> Result<tiny_http::Server> {
        // Ephemeral ports always succeed; skip the cancel-retry dance entirely.
        if config.preferred_port == 0 {
            let addr = format!("{}:0", config.host);
            return tiny_http::Server::http(&addr).map_err(|err| {
                anyhow!(err).context(format!(
                    "Failed to bind loopback listener for OAuth callback on {addr}"
                ))
            });
        }

        match try_bind_with_cancel(config.host, config.preferred_port) {
            Ok(server) => Ok(server),
            Err(primary_err) => {
                let Some(fallback_port) = config.fallback_port else {
                    return Err(primary_err.context(format!(
                        "Failed to bind loopback listener for OAuth callback on {}:{}",
                        config.host, config.preferred_port,
                    )));
                };
                log::warn!(
                    "OAuth callback port {}:{} unavailable; falling back to port {}",
                    config.host,
                    config.preferred_port,
                    fallback_port,
                );
                try_bind_with_cancel(config.host, fallback_port).map_err(|fallback_err| {
                    fallback_err.context(format!(
                        "Failed to bind loopback listener for OAuth callback on {}:{} or {}:{}",
                        config.host, config.preferred_port, config.host, fallback_port,
                    ))
                })
            }
        }
    }

    /// Attempts to bind to a fixed `host:port`. On `AddrInUse`, sends a single
    /// `GET /cancel` to the existing listener (to evict a previous OAuth flow
    /// from this or a compatible client) and retries.
    fn try_bind_with_cancel(host: &'static str, port: u16) -> Result<tiny_http::Server> {
        let addr = format!("{host}:{port}");
        let mut cancel_attempted = false;
        let mut last_err: Option<anyhow::Error> = None;

        for _ in 0..BIND_MAX_ATTEMPTS {
            match tiny_http::Server::http(&addr) {
                Ok(server) => return Ok(server),
                Err(err) => {
                    let is_addr_in_use = err
                        .downcast_ref::<std::io::Error>()
                        .map(|io_err| io_err.kind() == std::io::ErrorKind::AddrInUse)
                        .unwrap_or(false);

                    if !is_addr_in_use {
                        return Err(anyhow!(err).context(format!(
                            "Failed to bind loopback listener for OAuth callback on {addr}"
                        )));
                    }

                    if !cancel_attempted {
                        cancel_attempted = true;
                        if let Err(cancel_err) = send_cancel_request(host, port) {
                            log::warn!(
                                "Failed to cancel previous OAuth callback server on {addr}: {cancel_err}"
                            );
                        }
                    }

                    last_err = Some(anyhow!(err));
                    std::thread::sleep(BIND_RETRY_DELAY);
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| anyhow!("unknown bind error"))
            .context(format!(
                "OAuth callback port {addr} remained in use after {BIND_MAX_ATTEMPTS} attempts"
            )))
    }

    /// Sends `GET /cancel` to a listener on `host:port`, asking it to shut down.
    ///
    /// Best-effort: errors here are surfaced to the caller for logging but do
    /// not block the subsequent rebind attempt.
    fn send_cancel_request(host: &str, port: u16) -> std::io::Result<()> {
        use std::io::{Read as _, Write as _};
        use std::net::{TcpStream, ToSocketAddrs as _};

        let addr = format!("{host}:{port}")
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("could not resolve {host}:{port}"),
                )
            })?;
        let mut stream = TcpStream::connect_timeout(&addr, CANCEL_REQUEST_TIMEOUT)?;
        stream.set_read_timeout(Some(CANCEL_REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(CANCEL_REQUEST_TIMEOUT))?;

        stream.write_all(b"GET /cancel HTTP/1.1\r\n")?;
        stream.write_all(format!("Host: {host}:{port}\r\n").as_bytes())?;
        stream.write_all(b"Connection: close\r\n\r\n")?;

        // Drain the response so the server can close cleanly. We don't care
        // about the body; errors here are harmless.
        let mut buf = [0u8; 64];
        let _ = stream.read(&mut buf);
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
pub use server::{
    OAuthCallbackParams, OAuthCallbackServerConfig, start_oauth_callback_server,
    start_oauth_callback_server_with_config,
};

#[cfg(not(target_family = "wasm"))]
pub fn codex_callback_server_config() -> OAuthCallbackServerConfig {
    OAuthCallbackServerConfig {
        host: CODEX_CALLBACK_HOST,
        preferred_port: CODEX_CALLBACK_PORT,
        fallback_port: Some(CODEX_CALLBACK_FALLBACK_PORT),
        path: CODEX_CALLBACK_PATH,
    }
}
