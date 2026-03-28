use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
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

pub async fn auth_method(Json(body): Json<AuthMethodRequest>) -> impl IntoResponse {
    let _ = body.username;
    Json(serde_json::json!({ "method": "password" }))
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

    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, password_hash, role FROM users WHERE username = ?",
    )
    .bind(&body.username)
    .fetch_optional(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let (user_id, password_hash, _role) = match row {
        Some(r) => r,
        None => {
            let _ = state.brute.record_failure(&ip, &body.username);
            return Err(AppError::unauthorized("invalid credentials"));
        }
    };

    let pw = body.password.clone();
    let hash = password_hash.clone();
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
    state.jwt.store_refresh_token(&state.db, &refresh_claims).await.map_err(AppError::internal)?;

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
    let now = chrono::Utc::now().timestamp();

    let username = body.username.clone();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at) VALUES (?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&username)
    .bind(&hash)
    .bind("user")
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await
    .map_err(|e| {
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
        .rotate(&state.db, &body.refresh_token, body.device_id.as_deref())
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
