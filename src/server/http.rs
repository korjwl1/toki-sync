use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::{BruteForceGuard, JwtManager};
use crate::db::Database;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub jwt: Arc<JwtManager>,
    pub brute: Arc<BruteForceGuard>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth-method", post(auth_method))
        .route("/login", post(login))
        .route("/token/refresh", post(token_refresh))
        .with_state(state)
}

// ─── /health ────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// ─── /auth-method ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AuthMethodRequest {
    username: String,
}

#[derive(Serialize)]
struct AuthMethodResponse {
    method: String,
}

async fn auth_method(
    Json(body): Json<AuthMethodRequest>,
) -> impl IntoResponse {
    // Currently only password auth is supported.
    // In the future this could return "totp", "sso", etc.
    let _ = body.username;
    Json(AuthMethodResponse { method: "password".to_string() })
}

// ─── /login ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    /// Optional: associate refresh token with a device
    device_id: Option<String>,
}

#[derive(Serialize)]
struct LoginResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: u64,
}

async fn login(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    let ip = addr.ip().to_string();

    // Check brute force lockout before even touching DB
    state.brute.check(&ip, &body.username).map_err(|secs| {
        AppError::locked_out(secs)
    })?;

    // Look up user
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

    // Verify password (bcrypt — blocking, run in threadpool)
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

    // Issue tokens
    let access = state.jwt.issue_access_token(&user_id)
        .map_err(AppError::internal)?;
    let (refresh, refresh_claims) = state.jwt.issue_refresh_token(
        &user_id,
        body.device_id.as_deref(),
    ).map_err(AppError::internal)?;

    state.jwt.store_refresh_token(&state.db, &refresh_claims)
        .await
        .map_err(AppError::internal)?;

    tracing::info!(user_id = %user_id, "login successful");

    Ok(Json(LoginResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_string(),
        expires_in: 3600,
    }))
}

// ─── /token/refresh ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RefreshRequest {
    refresh_token: String,
    device_id: Option<String>,
}

#[derive(Serialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: u64,
}

async fn token_refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, AppError> {
    let (access, refresh) = state.jwt
        .rotate(&state.db, &body.refresh_token, body.device_id.as_deref())
        .await
        .map_err(|_| AppError::unauthorized("invalid or expired refresh token"))?;

    Ok(Json(RefreshResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_string(),
        expires_in: 3600,
    }))
}

// ─── Error type ─────────────────────────────────────────────────────────────

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".to_string(),
        }
    }

    fn unauthorized(msg: &str) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: msg.to_string() }
    }

    fn locked_out(retry_after: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("too many attempts, retry after {retry_after}s"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}
