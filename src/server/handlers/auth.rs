use axum::{
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::super::http::{AppError, AppState, extract_client_ip, extract_jwt, get_oidc_discovery, validate_username};

// ─── /health ────────────────────────────────────────────────────────────────

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// ─── /auth-method ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AuthMethodRequest {
    pub username: String,
}

pub async fn auth_method(
    State(state): State<AppState>,
    Json(body): Json<AuthMethodRequest>,
) -> impl IntoResponse {
    let _ = body.username;
    let oidc_issuer = state.dynamic_settings.oidc_issuer().await;
    if !oidc_issuer.is_empty() {
        let oidc_redirect_uri = state.dynamic_settings.oidc_redirect_uri().await;
        // Build OIDC authorize URL for the client to redirect to
        let auth_url = format!(
            "/auth/oidc/authorize?redirect_uri={}",
            urlencoding::encode(&oidc_redirect_uri),
        );
        Json(serde_json::json!({ "method": "oidc", "auth_url": auth_url }))
    } else {
        Json(serde_json::json!({ "method": "password" }))
    }
}

// ─── /login ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub device_id: Option<String>,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}

pub async fn login(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    let ip = extract_client_ip(&headers, &addr, state.trust_proxy);

    state.brute.check(&ip, &body.username).map_err(AppError::locked_out)?;

    let user = state.db.get_user_by_username(&body.username).await.map_err(AppError::internal)?;

    let user = match user {
        Some(u) => u,
        None => {
            let _ = state.brute.record_failure(&ip, &body.username);
            return Err(AppError::unauthorized("invalid credentials"));
        }
    };

    // Check active status
    if !user.active {
        return Err(AppError::unauthorized("account deactivated"));
    }

    let user_id = user.id;
    let password_hash = user.password_hash;

    if password_hash.is_empty() {
        return Err(AppError::unauthorized("this account uses OIDC login"));
    }

    let pw = body.password.clone();
    let hash = password_hash;
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(&pw, &hash))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    if !valid {
        let _ = state.brute.record_failure(&ip, &body.username);
        return Err(AppError::unauthorized("invalid credentials"));
    }

    state.brute.record_success(&ip, &body.username);

    let access = state.jwt.issue_access_token(&user_id).map_err(AppError::internal)?;
    let (refresh, refresh_claims) = state.jwt
        .issue_refresh_token(&user_id, body.device_id.as_deref())
        .map_err(AppError::internal)?;
    state.jwt.store_refresh_token(&*state.db, &refresh_claims).await.map_err(AppError::internal)?;

    tracing::info!(user_id = %user_id, "login successful");
    Ok(Json(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_string(),
        expires_in: state.access_token_ttl_secs,
    }))
}

// ─── /register ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

pub async fn register(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let ip = extract_client_ip(&headers, &addr, state.trust_proxy);
    state.brute.check(&ip, "__register__").map_err(AppError::locked_out)?;

    let mode = state.dynamic_settings.registration_mode().await;
    match mode.as_str() {
        "open" | "approval" => { /* allowed */ }
        _ => {
            return Err(AppError::forbidden("registration is disabled"));
        }
    }

    validate_username(&body.username)?;

    if body.password.len() < 8 || body.password.len() > 128 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be 8-128 characters".into() });
    }

    let pw = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let id = uuid::Uuid::new_v4().to_string();
    let username = body.username.clone();

    if mode.as_str() == "approval" {
        // Insert into pending_registrations, not users
        state.db.create_pending_registration(&id, &username, &hash).await.map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                state.brute.record_failure(&ip, "__register__").ok();
                AppError::conflict("username already exists or pending")
            } else {
                AppError::internal(e)
            }
        })?;

        state.brute.record_success(&ip, "__register__");
        return Ok((StatusCode::ACCEPTED, Json(serde_json::json!({
            "message": "registration pending admin approval"
        }))));
    }

    // "open" mode: create user immediately
    let new_user = crate::db::models::NewUser {
        id: id.clone(),
        username: username.clone(),
        password_hash: hash,
        role: "user".to_string(),
    };
    state.db.create_user(&new_user).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            state.brute.record_failure(&ip, "__register__").ok();
            AppError::conflict("username already exists")
        } else {
            AppError::internal(e)
        }
    })?;

    state.brute.record_success(&ip, "__register__");
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id, "username": username }))))
}

// ─── /token/refresh ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
    pub device_id: Option<String>,
}

pub async fn token_refresh(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    let ip = extract_client_ip(&headers, &addr, state.trust_proxy);
    state.brute.check(&ip, "__refresh__").map_err(AppError::locked_out)?;

    let result = state.jwt
        .rotate(&*state.db, &body.refresh_token, body.device_id.as_deref())
        .await;

    match result {
        Ok((access, refresh)) => {
            state.brute.record_success(&ip, "__refresh__");
            Ok(Json(TokenResponse {
                access_token: access,
                refresh_token: refresh,
                token_type: "Bearer".to_string(),
                expires_in: state.access_token_ttl_secs,
            }))
        }
        Err(_) => {
            let _ = state.brute.record_failure(&ip, "__refresh__");
            Err(AppError::unauthorized("invalid or expired refresh token"))
        }
    }
}

// ─── OIDC /auth/oidc/authorize ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct OidcAuthorizeQuery {
    #[serde(default)]
    pub redirect_uri: String,
}

pub async fn oidc_authorize(
    State(state): State<AppState>,
    Query(params): Query<OidcAuthorizeQuery>,
) -> Result<Redirect, AppError> {
    let oidc_issuer = state.dynamic_settings.oidc_issuer().await;
    if oidc_issuer.is_empty() {
        return Err(AppError { status: StatusCode::NOT_FOUND, message: "OIDC not configured".into() });
    }

    // Discover OIDC provider endpoints (cached with TTL)
    let discovery = get_oidc_discovery(&state).await?;

    // Generate CSRF state token and nonce
    let csrf_state = uuid::Uuid::new_v4().to_string();
    let nonce = uuid::Uuid::new_v4().to_string();

    // Store the state token with the client's desired redirect_uri and nonce (for CLI flow)
    let client_redirect = if params.redirect_uri.is_empty() {
        String::new()
    } else {
        params.redirect_uri.clone()
    };
    state.oidc_state_store.insert(csrf_state.clone(), client_redirect, nonce.clone());

    let oidc_client_id = state.dynamic_settings.oidc_client_id().await;
    let oidc_redirect_uri = state.dynamic_settings.oidc_redirect_uri().await;

    // Build the authorization URL
    let auth_url = crate::auth::oidc::build_auth_url(
        &discovery,
        &oidc_client_id,
        &oidc_redirect_uri,
        &csrf_state,
        &nonce,
    );

    Ok(Redirect::temporary(&auth_url))
}

// ─── OIDC /auth/callback ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct OidcCallbackQuery {
    pub code: String,
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    Query(params): Query<OidcCallbackQuery>,
) -> Result<axum::response::Response, AppError> {
    let oidc_issuer = state.dynamic_settings.oidc_issuer().await;
    if oidc_issuer.is_empty() {
        return Err(AppError { status: StatusCode::NOT_FOUND, message: "OIDC not configured".into() });
    }

    // Check for error from provider
    if let Some(ref err) = params.error {
        return Err(AppError { status: StatusCode::BAD_REQUEST, message: format!("OIDC error: {err}") });
    }

    // Validate CSRF state and retrieve nonce
    let (client_redirect, stored_nonce) = state.oidc_state_store.validate(&params.state)
        .ok_or_else(|| AppError { status: StatusCode::BAD_REQUEST, message: "invalid or expired state parameter".into() })?;

    // Discover provider endpoints (cached with TTL)
    let discovery = get_oidc_discovery(&state).await?;

    let oidc_client_id = state.dynamic_settings.oidc_client_id().await;
    let oidc_client_secret = state.dynamic_settings.oidc_client_secret().await;
    let oidc_redirect_uri = state.dynamic_settings.oidc_redirect_uri().await;

    // Exchange code for tokens
    let token_resp = crate::auth::oidc::exchange_code(
        &discovery,
        &oidc_client_id,
        &oidc_client_secret,
        &oidc_redirect_uri,
        &params.code,
        &state.oidc_http_client,
    )
    .await
    .map_err(AppError::internal)?;

    // Extract user info (with nonce validation)
    let nonce_ref = if stored_nonce.is_empty() { None } else { Some(stored_nonce.as_str()) };
    let user_info = crate::auth::oidc::extract_user_info(
        &token_resp,
        &discovery,
        &oidc_issuer,
        &oidc_client_id,
        nonce_ref,
        &state.oidc_http_client,
    )
        .await
        .map_err(AppError::internal)?;

    // Find or create user by OIDC subject
    let user = state.db.find_user_by_oidc(&oidc_issuer, &user_info.sub)
        .await
        .map_err(AppError::internal)?;

    let user_id = match user {
        Some(u) => {
            // Check active status for existing OIDC users
            if !u.active {
                return Err(AppError::unauthorized("account deactivated"));
            }
            u.id
        }
        None => {
            // Create new OIDC user
            let id = uuid::Uuid::new_v4().to_string();
            let username = user_info.email.clone()
                .or_else(|| user_info.name.clone())
                .unwrap_or_else(|| format!("oidc_{}", &user_info.sub[..8.min(user_info.sub.len())]));

            let new_user = crate::db::models::NewOidcUser {
                id: id.clone(),
                username,
                role: "user".to_string(),
                oidc_sub: user_info.sub.clone(),
                oidc_issuer: oidc_issuer.clone(),
            };
            match state.db.create_oidc_user(&new_user).await {
                Ok(()) => {
                    tracing::info!(oidc_sub = %user_info.sub, "OIDC user created");
                    id
                }
                Err(e) => {
                    // On UNIQUE constraint violation (concurrent creation), retry find
                    if e.to_string().contains("UNIQUE") {
                        let retry_user = state.db.find_user_by_oidc(&oidc_issuer, &user_info.sub)
                            .await
                            .map_err(AppError::internal)?
                            .ok_or_else(|| AppError::internal(anyhow::anyhow!(
                                "OIDC user creation conflict but user not found on retry"
                            )))?;
                        if !retry_user.active {
                            return Err(AppError::unauthorized("account deactivated"));
                        }
                        retry_user.id
                    } else {
                        return Err(AppError::internal(e));
                    }
                }
            }
        }
    };

    // Issue JWT pair
    let access = state.jwt.issue_access_token(&user_id).map_err(AppError::internal)?;
    let (refresh, refresh_claims) = state.jwt
        .issue_refresh_token(&user_id, None)
        .map_err(AppError::internal)?;
    state.jwt.store_refresh_token(&*state.db, &refresh_claims)
        .await
        .map_err(AppError::internal)?;

    tracing::info!(user_id = %user_id, "OIDC login successful");

    // If client provided a redirect_uri (CLI flow), redirect with tokens.
    // Only allow localhost redirects with tokens in query params (safe: localhost
    // traffic is not proxied or logged externally). Reject non-localhost redirects.
    if !client_redirect.is_empty() {
        if client_redirect.starts_with("http://127.0.0.1") || client_redirect.starts_with("http://localhost") {
            let redirect_url = format!(
                "{}?access_token={}&refresh_token={}&token_type=Bearer&expires_in={}",
                client_redirect,
                urlencoding::encode(&access),
                urlencoding::encode(&refresh),
                state.access_token_ttl_secs,
            );
            return Ok(Redirect::temporary(&redirect_url).into_response());
        } else {
            return Err(AppError {
                status: StatusCode::BAD_REQUEST,
                message: "OIDC CLI redirect must be localhost (http://127.0.0.1 or http://localhost)".into(),
            });
        }
    }

    // Browser flow: redirect to admin panel with tokens in URL fragment
    let admin_url = format!(
        "/admin#access_token={}&refresh_token={}&expires_in={}",
        urlencoding::encode(&access),
        urlencoding::encode(&refresh),
        state.access_token_ttl_secs,
    );
    Ok(Redirect::temporary(&admin_url).into_response())
}

// ─── GET /auth/info ────────────────────────────────────────────────────────

pub async fn auth_info(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let reg_mode = state.dynamic_settings.registration_mode().await;
    let oidc_issuer = state.dynamic_settings.oidc_issuer().await;
    Json(serde_json::json!({
        "registration_mode": reg_mode,
        "oidc_enabled": !oidc_issuer.is_empty(),
    }))
}

// ─── Device Authorization Grant (RFC 8628) ─────────────────────────────────

/// Characters for user_code generation (uppercase alphanumeric, excluding ambiguous chars)
const USER_CODE_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn generate_user_code() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut code = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            code.push('-');
        }
        code.push(USER_CODE_CHARS[bytes[i] as usize % USER_CODE_CHARS.len()] as char);
    }
    code
}

/// POST /device/code — initiate device authorization
pub async fn device_code_request(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let device_code = uuid::Uuid::new_v4().to_string();
    let user_code = generate_user_code();
    let expires_at = chrono::Utc::now().timestamp() + 300; // 5 minutes

    state.db.create_device_code(&device_code, &user_code, expires_at)
        .await
        .map_err(AppError::internal)?;

    let verification_url = if state.external_url.is_empty() {
        "/login".to_string()
    } else {
        format!("{}/login/device", state.external_url)
    };

    Ok(Json(serde_json::json!({
        "device_code": device_code,
        "user_code": user_code,
        "verification_url": verification_url,
        "expires_in": 300,
        "interval": 5,
    })))
}

/// POST /device/token — poll for device authorization result
#[derive(Deserialize)]
pub struct DeviceTokenRequest {
    pub device_code: String,
    /// Stable device key UUID (sent by client to register device on approval)
    pub device_key: Option<String>,
    /// Human-readable device name (e.g. hostname)
    pub device_name: Option<String>,
}

pub async fn device_token_poll(
    State(state): State<AppState>,
    Json(body): Json<DeviceTokenRequest>,
) -> Result<axum::response::Response, AppError> {
    // Rate limit: if client polls faster than 5s, return slow_down
    {
        let mut tracker = state.device_poll_tracker.lock().unwrap();
        // Sweep entries older than 15 minutes (device codes expire in 10min)
        tracker.retain(|_, instant| instant.elapsed() < std::time::Duration::from_secs(900));
        if let Some(last) = tracker.get(&body.device_code) {
            if last.elapsed() < std::time::Duration::from_secs(5) {
                return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                    "error": "slow_down",
                    "interval": 10
                }))).into_response());
            }
        }
        tracker.insert(body.device_code.clone(), std::time::Instant::now());
    }

    let dc = state.db.get_device_code(&body.device_code)
        .await
        .map_err(AppError::internal)?;

    let dc = match dc {
        Some(dc) => dc,
        None => {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "expired_token" }))).into_response());
        }
    };

    let now = chrono::Utc::now().timestamp();
    if now > dc.expires_at {
        // Clean up expired code
        let _ = state.db.delete_device_code(&body.device_code).await;
        return Err(AppError { status: StatusCode::GONE, message: "expired_token".into() });
    }

    if dc.approved_by.is_none() {
        // Still pending
        return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "authorization_pending"
        }))).into_response());
    }

    // Approved — return tokens and delete the device code row
    let access_token = dc.access_token.unwrap_or_default();
    let refresh_token = dc.refresh_token.unwrap_or_default();
    let user_id = dc.approved_by.unwrap_or_default();

    let _ = state.db.delete_device_code(&body.device_code).await;

    // Clean up poll tracker entry now that the device code is consumed
    state.device_poll_tracker.lock().unwrap().remove(&body.device_code);

    // Register device if device_key was provided (new flow: device creation at token exchange)
    if let Some(ref device_key) = body.device_key {
        if !device_key.is_empty() && !user_id.is_empty() {
            let device_name = body.device_name.as_deref().unwrap_or("unknown");
            // Truncate to 64 chars
            let device_name = if device_name.len() > 64 {
                &device_name[..device_name.char_indices().nth(64).map_or(device_name.len(), |(i, _)| i)]
            } else {
                device_name
            };
            // Find or create — idempotent in case of retry
            let existing = state.db.find_device_by_key_and_user(device_key, &user_id).await;
            if let Ok(None) = existing {
                // Use device_key as ID (client's stable UUID)
                if let Err(e) = state.db.create_device(device_key, &user_id, device_name, device_key).await {
                    tracing::warn!("failed to register device during token exchange: {e}");
                } else {
                    tracing::info!(
                        user_id = %user_id,
                        device_key = %device_key,
                        device_name = %device_name,
                        "registered new device during token exchange"
                    );
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": "Bearer",
        "expires_in": state.access_token_ttl_secs,
    })).into_response())
}

/// POST /device/approve — approve a device code (JWT required)
#[derive(Deserialize)]
pub struct DeviceApproveRequest {
    pub user_code: String,
}

pub async fn device_approve(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<DeviceApproveRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let user_id = &claims.sub;

    state.brute.check(user_id, "__device_approve__").map_err(AppError::locked_out)?;

    // Normalize user_code: uppercase, ensure hyphen
    let user_code = body.user_code.to_uppercase().replace(' ', "");
    let user_code = if user_code.len() == 8 && !user_code.contains('-') {
        format!("{}-{}", &user_code[..4], &user_code[4..])
    } else {
        user_code
    };

    // Verify the device code exists and is not expired
    let dc = state.db.get_device_code_by_user_code(&user_code)
        .await
        .map_err(AppError::internal)?;

    let dc = match dc {
        Some(dc) => dc,
        None => {
            state.brute.record_failure(user_id, "__device_approve__").ok();
            return Err(AppError::not_found("invalid or expired code"));
        }
    };

    let now = chrono::Utc::now().timestamp();
    if now > dc.expires_at {
        let _ = state.db.delete_device_code(&dc.device_code).await;
        return Err(AppError { status: StatusCode::GONE, message: "code expired".into() });
    }

    if dc.approved_by.is_some() {
        return Err(AppError::conflict("code already approved"));
    }

    // Issue JWT pair for the user
    let access = state.jwt.issue_access_token(user_id).map_err(AppError::internal)?;
    let (refresh, refresh_claims) = state.jwt
        .issue_refresh_token(user_id, None)
        .map_err(AppError::internal)?;
    state.jwt.store_refresh_token(&*state.db, &refresh_claims)
        .await
        .map_err(AppError::internal)?;

    // Store tokens in the device code row
    let approved = state.db.approve_device_code(&user_code, user_id, &access, &refresh)
        .await
        .map_err(AppError::internal)?;

    if !approved {
        // Revoke the orphaned refresh token we just created
        let _ = state.db.revoke_refresh_token(&refresh_claims.jti).await;
        return Err(AppError::conflict("code already approved or expired"));
    }

    state.brute.record_success(user_id, "__device_approve__");
    tracing::info!(user_id = %user_id, user_code = %user_code, "device code approved");

    Ok(Json(serde_json::json!({ "status": "approved" })))
}


