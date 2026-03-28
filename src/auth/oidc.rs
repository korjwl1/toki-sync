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
    #[serde(default)]
    pub jwks_uri: String,
}

/// A single JSON Web Key from the JWKS endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct JwkKey {
    #[serde(default)]
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(rename = "use", default)]
    pub use_: Option<String>,
    // RSA components
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
}

/// JWKS (JSON Web Key Set) response.
#[derive(Debug, Clone, Deserialize)]
pub struct JwksResponse {
    pub keys: Vec<JwkKey>,
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
/// Maps state token -> (redirect_uri, nonce, created_at) with TTL.
pub struct OidcStateStore {
    states: Mutex<HashMap<String, (String, String, Instant)>>,
    ttl: Duration,
}

impl OidcStateStore {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Store a state token with its associated redirect_uri and nonce.
    pub fn insert(&self, state: String, redirect_uri: String, nonce: String) {
        let mut map = self.states.lock().unwrap();
        // Cleanup expired entries
        let now = Instant::now();
        map.retain(|_, (_, _, created)| now.duration_since(*created) < self.ttl);
        map.insert(state, (redirect_uri, nonce, now));
    }

    /// Validate and consume a state token. Returns (redirect_uri, nonce) if valid.
    pub fn validate(&self, state: &str) -> Option<(String, String)> {
        let mut map = self.states.lock().unwrap();
        if let Some((redirect_uri, nonce, created)) = map.remove(state) {
            let now = Instant::now();
            if now.duration_since(created) < self.ttl {
                return Some((redirect_uri, nonce));
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

/// Fetch the JWKS (JSON Web Key Set) from the provider's jwks_uri.
pub async fn fetch_jwks(jwks_uri: &str) -> Result<Vec<JwkKey>> {
    let client = reqwest::Client::new();
    let resp = client.get(jwks_uri)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("JWKS fetch failed: {jwks_uri}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("JWKS endpoint returned status {}", resp.status()));
    }

    let jwks: JwksResponse = resp.json().await
        .context("failed to parse JWKS response")?;
    Ok(jwks.keys)
}

/// Build the authorization URL for redirecting the user to the OIDC provider.
pub fn build_auth_url(
    discovery: &OidcDiscovery,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}",
        discovery.authorization_endpoint,
        urlencoding::encode(client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode("openid email profile"),
        urlencoding::encode(state),
        urlencoding::encode(nonce),
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

/// Extract user info from the id_token with JWKS-based signature verification.
/// Falls back to the userinfo endpoint if id_token is empty or JWKS verification
/// is not available.
///
/// When JWKS is available:
/// - Decodes JWT header to find the `kid` (key ID)
/// - Finds the matching RSA key in the JWKS
/// - Verifies the signature using `jsonwebtoken::decode()`
/// - Validates iss, aud, exp claims via `jsonwebtoken::Validation`
///
/// When JWKS is not available (empty jwks_uri or no matching key), falls back to
/// manual parsing with basic claim validation.
pub async fn extract_user_info(
    token_resp: &TokenResponse,
    discovery: &OidcDiscovery,
    issuer_url: &str,
    client_id: &str,
    expected_nonce: Option<&str>,
) -> Result<OidcUserInfo> {
    // Try id_token first
    if !token_resp.id_token.is_empty() {
        // Attempt JWKS-based verification if jwks_uri is available
        if !discovery.jwks_uri.is_empty() {
            match verify_id_token_with_jwks(
                &token_resp.id_token,
                &discovery.jwks_uri,
                issuer_url,
                client_id,
                expected_nonce,
            ).await {
                Ok(info) => return Ok(info),
                Err(e) => {
                    tracing::warn!("JWKS verification failed, falling back to userinfo: {e}");
                    // Fall through to userinfo endpoint
                }
            }
        } else {
            // No jwks_uri: parse without cryptographic verification (received over TLS)
            if let Ok(info) = parse_id_token_claims(&token_resp.id_token) {
                validate_id_token_claims(&token_resp.id_token, issuer_url, client_id)?;
                if let Some(expected) = expected_nonce {
                    validate_id_token_nonce(&token_resp.id_token, expected)?;
                }
                return Ok(info);
            }
        }
    }

    // Fall back to userinfo endpoint
    if !discovery.userinfo_endpoint.is_empty() {
        return fetch_userinfo(&token_resp.access_token, &discovery.userinfo_endpoint).await;
    }

    Err(anyhow!("no id_token and no userinfo endpoint available"))
}

/// Verify and decode an id_token using JWKS-based RSA signature verification.
async fn verify_id_token_with_jwks(
    id_token: &str,
    jwks_uri: &str,
    issuer_url: &str,
    client_id: &str,
    expected_nonce: Option<&str>,
) -> Result<OidcUserInfo> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};

    // Decode the JWT header to get the kid and algorithm
    let header = decode_header(id_token)
        .context("failed to decode id_token JWT header")?;

    let kid = header.kid
        .ok_or_else(|| anyhow!("id_token JWT header missing kid"))?;

    // Fetch JWKS
    let keys = fetch_jwks(jwks_uri).await?;

    // Find the matching key
    let jwk = keys.iter()
        .find(|k| k.kid.as_deref() == Some(&kid))
        .ok_or_else(|| anyhow!("no matching key found in JWKS for kid={kid}"))?;

    // Only RSA keys are supported for now
    if jwk.kty != "RSA" {
        return Err(anyhow!("unsupported key type: {} (only RSA supported)", jwk.kty));
    }

    let n = jwk.n.as_deref()
        .ok_or_else(|| anyhow!("JWKS key missing 'n' component"))?;
    let e = jwk.e.as_deref()
        .ok_or_else(|| anyhow!("JWKS key missing 'e' component"))?;

    let decoding_key = DecodingKey::from_rsa_components(n, e)
        .context("failed to build RSA decoding key from JWKS components")?;

    // Determine algorithm from header (default RS256)
    let algorithm = match header.alg {
        jsonwebtoken::Algorithm::RS256 => Algorithm::RS256,
        jsonwebtoken::Algorithm::RS384 => Algorithm::RS384,
        jsonwebtoken::Algorithm::RS512 => Algorithm::RS512,
        other => return Err(anyhow!("unsupported JWT algorithm: {:?}", other)),
    };

    // Build validation: check iss, aud, exp
    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[issuer_url]);
    validation.set_audience(&[client_id]);
    validation.validate_exp = true;

    // Decode and verify
    let token_data = decode::<IdTokenClaimsWithNonce>(id_token, &decoding_key, &validation)
        .context("id_token signature or claim validation failed")?;

    let claims = token_data.claims;

    // Validate nonce if expected
    if let Some(expected) = expected_nonce {
        match &claims.nonce {
            Some(token_nonce) if token_nonce == expected => {},
            Some(token_nonce) => {
                return Err(anyhow!("nonce mismatch: expected {expected}, got {token_nonce}"));
            },
            None => {
                return Err(anyhow!("id_token missing nonce claim"));
            }
        }
    }

    Ok(OidcUserInfo {
        sub: claims.sub,
        email: claims.email,
        name: claims.name,
    })
}

/// ID token claims including nonce for JWKS-verified path.
#[derive(Debug, Deserialize)]
struct IdTokenClaimsWithNonce {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
}

/// Validate basic claims (iss, aud, exp) of an id_token JWT.
fn validate_id_token_claims(id_token: &str, issuer_url: &str, client_id: &str) -> Result<()> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return Ok(()); // Not a valid JWT structure; skip validation (parse already failed above)
    }

    let payload_bytes = base64_decode::decode_url_safe(parts[1])?;
    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .context("failed to parse id_token claims for validation")?;

    if let Some(iss) = claims.get("iss").and_then(|v| v.as_str()) {
        if iss != issuer_url {
            anyhow::bail!("id_token issuer mismatch: expected {issuer_url}, got {iss}");
        }
    }

    if let Some(aud) = claims.get("aud") {
        let valid = match aud {
            serde_json::Value::String(s) => s == client_id,
            serde_json::Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some(client_id)),
            _ => false,
        };
        if !valid {
            anyhow::bail!("id_token audience does not contain client_id {client_id}");
        }
    }

    if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
        let now = chrono::Utc::now().timestamp();
        if exp < now {
            anyhow::bail!("id_token expired");
        }
    }

    Ok(())
}

/// Validate that the nonce claim in the id_token matches the expected nonce.
fn validate_id_token_nonce(id_token: &str, expected_nonce: &str) -> Result<()> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return Ok(());
    }

    let payload_bytes = base64_decode::decode_url_safe(parts[1])?;
    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .context("failed to parse id_token claims for nonce validation")?;

    if let Some(token_nonce) = claims.get("nonce").and_then(|v| v.as_str()) {
        if token_nonce != expected_nonce {
            anyhow::bail!("nonce mismatch");
        }
    }

    Ok(())
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
