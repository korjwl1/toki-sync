use axum::{
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::super::http::{AppError, AppState, extract_client_ip};

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
    if !state.oidc_issuer.is_empty() {
        // Build OIDC authorize URL for the client to redirect to
        let auth_url = format!(
            "/auth/oidc/authorize?redirect_uri={}",
            urlencoding::encode(&state.oidc_redirect_uri),
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
    let ip = extract_client_ip(&headers, &addr);

    state.brute.check(&ip, &body.username).map_err(AppError::locked_out)?;

    let user = state.db.get_user_by_username(&body.username).await.map_err(AppError::internal)?;

    let user = match user {
        Some(u) => u,
        None => {
            let _ = state.brute.record_failure(&ip, &body.username);
            return Err(AppError::unauthorized("invalid credentials"));
        }
    };

    let user_id = user.id;
    let password_hash = user.password_hash;

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
    let ip = extract_client_ip(&headers, &addr);
    state.brute.check(&ip, "__register__").map_err(AppError::locked_out)?;

    if !state.allow_registration {
        return Err(AppError { status: StatusCode::FORBIDDEN, message: "registration is disabled".into() });
    }

    if body.password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let pw = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let id = uuid::Uuid::new_v4().to_string();

    let username = body.username.clone();
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
    let ip = extract_client_ip(&headers, &addr);
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
    if state.oidc_issuer.is_empty() {
        return Err(AppError { status: StatusCode::NOT_FOUND, message: "OIDC not configured".into() });
    }

    // Discover OIDC provider endpoints
    let discovery = crate::auth::oidc::discover(&state.oidc_issuer)
        .await
        .map_err(AppError::internal)?;

    // Generate CSRF state token
    let csrf_state = uuid::Uuid::new_v4().to_string();

    // Store the state token with the client's desired redirect_uri (for CLI flow)
    let client_redirect = if params.redirect_uri.is_empty() {
        String::new()
    } else {
        params.redirect_uri.clone()
    };
    state.oidc_state_store.insert(csrf_state.clone(), client_redirect);

    // Build the authorization URL
    let auth_url = crate::auth::oidc::build_auth_url(
        &discovery,
        &state.oidc_client_id,
        &state.oidc_redirect_uri,
        &csrf_state,
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
    if state.oidc_issuer.is_empty() {
        return Err(AppError { status: StatusCode::NOT_FOUND, message: "OIDC not configured".into() });
    }

    // Check for error from provider
    if let Some(ref err) = params.error {
        return Err(AppError { status: StatusCode::BAD_REQUEST, message: format!("OIDC error: {err}") });
    }

    // Validate CSRF state
    let client_redirect = state.oidc_state_store.validate(&params.state)
        .ok_or_else(|| AppError { status: StatusCode::BAD_REQUEST, message: "invalid or expired state parameter".into() })?;

    // Discover provider endpoints
    let discovery = crate::auth::oidc::discover(&state.oidc_issuer)
        .await
        .map_err(AppError::internal)?;

    // Exchange code for tokens
    let token_resp = crate::auth::oidc::exchange_code(
        &discovery,
        &state.oidc_client_id,
        &state.oidc_client_secret,
        &state.oidc_redirect_uri,
        &params.code,
    )
    .await
    .map_err(AppError::internal)?;

    // Extract user info
    let user_info = crate::auth::oidc::extract_user_info(&token_resp, &discovery)
        .await
        .map_err(AppError::internal)?;

    // Find or create user by OIDC subject
    let user = state.db.find_user_by_oidc(&state.oidc_issuer, &user_info.sub)
        .await
        .map_err(AppError::internal)?;

    let user_id = match user {
        Some(u) => u.id,
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
                oidc_issuer: state.oidc_issuer.clone(),
            };
            state.db.create_oidc_user(&new_user).await.map_err(AppError::internal)?;
            tracing::info!(oidc_sub = %user_info.sub, "OIDC user created");
            id
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

    // If client provided a redirect_uri (CLI flow), redirect with tokens
    if !client_redirect.is_empty() {
        let redirect_url = format!(
            "{}?access_token={}&refresh_token={}&token_type=Bearer&expires_in={}",
            client_redirect,
            urlencoding::encode(&access),
            urlencoding::encode(&refresh),
            state.access_token_ttl_secs,
        );
        return Ok(Redirect::temporary(&redirect_url).into_response());
    }

    // Browser flow: redirect to dashboard with tokens in URL fragment
    let dashboard_url = format!(
        "/dashboard#access_token={}&refresh_token={}&expires_in={}",
        urlencoding::encode(&access),
        urlencoding::encode(&refresh),
        state.access_token_ttl_secs,
    );
    Ok(Redirect::temporary(&dashboard_url).into_response())
}
