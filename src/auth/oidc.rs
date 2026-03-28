//! OIDC (OpenID Connect) support: discovery, authorization URL, token exchange, user info.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cached OIDC provider discovery document.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcDiscovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub userinfo_endpoint: String,
    pub issuer: String,
}

/// Token response from the OIDC provider's token endpoint.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub id_token: String,
    #[serde(default)]
    pub token_type: String,
}

/// User info extracted from the OIDC id_token or userinfo endpoint.
#[derive(Debug, Clone)]
pub struct OidcUserInfo {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// ID token claims (minimal subset we care about).
#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Userinfo endpoint response.
#[derive(Debug, Deserialize)]
struct UserinfoResponse {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// OIDC state store for CSRF protection.
/// Maps state token -> (redirect_uri, created_at) with TTL.
pub struct OidcStateStore {
    states: Mutex<HashMap<String, (String, Instant)>>,
    ttl: Duration,
}

impl OidcStateStore {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Store a state token with its associated redirect_uri. Returns the state string.
    pub fn insert(&self, state: String, redirect_uri: String) {
        let mut map = self.states.lock().unwrap();
        // Cleanup expired entries
        let now = Instant::now();
        map.retain(|_, (_, created)| now.duration_since(*created) < self.ttl);
        map.insert(state, (redirect_uri, now));
    }

    /// Validate and consume a state token. Returns the redirect_uri if valid.
    pub fn validate(&self, state: &str) -> Option<String> {
        let mut map = self.states.lock().unwrap();
        if let Some((redirect_uri, created)) = map.remove(state) {
            let now = Instant::now();
            if now.duration_since(created) < self.ttl {
                return Some(redirect_uri);
            }
        }
        None
    }
}

/// Fetch the OIDC discovery document from the issuer's well-known endpoint.
pub async fn discover(issuer: &str) -> Result<OidcDiscovery> {
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client.get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("OIDC discovery request failed: {url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("OIDC discovery returned status {}", resp.status()));
    }

    let discovery: OidcDiscovery = resp.json().await
        .context("failed to parse OIDC discovery document")?;
    Ok(discovery)
}

/// Build the authorization URL for redirecting the user to the OIDC provider.
pub fn build_auth_url(
    discovery: &OidcDiscovery,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        discovery.authorization_endpoint,
        urlencoding::encode(client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode("openid email profile"),
        urlencoding::encode(state),
    )
}

/// Exchange an authorization code for tokens at the provider's token endpoint.
pub async fn exchange_code(
    discovery: &OidcDiscovery,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::new();
    let resp = client.post(&discovery.token_endpoint)
        .timeout(Duration::from_secs(10))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .context("OIDC token exchange request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("OIDC token exchange failed ({}): {}", status, body));
    }

    let token_resp: TokenResponse = resp.json().await
        .context("failed to parse OIDC token response")?;
    Ok(token_resp)
}

/// Extract user info from the id_token (JWT payload, no signature verification against
/// provider keys -- we trust it because we just received it over TLS from the token endpoint).
/// Falls back to the userinfo endpoint if id_token is empty or parsing fails.
pub async fn extract_user_info(
    token_resp: &TokenResponse,
    discovery: &OidcDiscovery,
) -> Result<OidcUserInfo> {
    // Try id_token first
    if !token_resp.id_token.is_empty() {
        if let Ok(info) = parse_id_token_claims(&token_resp.id_token) {
            return Ok(info);
        }
    }

    // Fall back to userinfo endpoint
    if !discovery.userinfo_endpoint.is_empty() {
        return fetch_userinfo(&token_resp.access_token, &discovery.userinfo_endpoint).await;
    }

    Err(anyhow!("no id_token and no userinfo endpoint available"))
}

/// Parse the payload of a JWT id_token without cryptographic verification.
/// We trust it because it was received directly from the token endpoint over TLS.
fn parse_id_token_claims(id_token: &str) -> Result<OidcUserInfo> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!("invalid id_token format"));
    }

    // Decode the payload (second part), using URL-safe base64 without padding
    use base64_decode::decode_url_safe;
    let payload_bytes = decode_url_safe(parts[1])?;
    let claims: IdTokenClaims = serde_json::from_slice(&payload_bytes)
        .context("failed to parse id_token claims")?;

    Ok(OidcUserInfo {
        sub: claims.sub,
        email: claims.email,
        name: claims.name,
    })
}

/// Fetch user info from the OIDC provider's userinfo endpoint.
async fn fetch_userinfo(access_token: &str, userinfo_endpoint: &str) -> Result<OidcUserInfo> {
    let client = reqwest::Client::new();
    let resp = client.get(userinfo_endpoint)
        .timeout(Duration::from_secs(10))
        .bearer_auth(access_token)
        .send()
        .await
        .context("userinfo request failed")?;

    if !resp.status().is_success() {
        return Err(anyhow!("userinfo endpoint returned status {}", resp.status()));
    }

    let info: UserinfoResponse = resp.json().await
        .context("failed to parse userinfo response")?;

    Ok(OidcUserInfo {
        sub: info.sub,
        email: info.email,
        name: info.name,
    })
}

/// Minimal base64 URL-safe decoder (no-padding).
mod base64_decode {
    use anyhow::{anyhow, Result};

    pub fn decode_url_safe(input: &str) -> Result<Vec<u8>> {
        // Convert URL-safe base64 to standard base64
        let standard: String = input.chars().map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        }).collect();

        // Add padding if needed
        let padded = match standard.len() % 4 {
            2 => format!("{standard}=="),
            3 => format!("{standard}="),
            0 => standard,
            _ => return Err(anyhow!("invalid base64 length")),
        };

        // Decode using a simple implementation
        decode_standard(&padded)
    }

    fn decode_standard(input: &str) -> Result<Vec<u8>> {
        let mut output = Vec::with_capacity(input.len() * 3 / 4);
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;

        for c in input.chars() {
            if c == '=' { break; }
            let val = match c {
                'A'..='Z' => (c as u32) - ('A' as u32),
                'a'..='z' => (c as u32) - ('a' as u32) + 26,
                '0'..='9' => (c as u32) - ('0' as u32) + 52,
                '+' => 62,
                '/' => 63,
                _ => return Err(anyhow!("invalid base64 char: {c}")),
            };
            buf = (buf << 6) | val;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }

        Ok(output)
    }
}
