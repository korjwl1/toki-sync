use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::{BruteForceGuard, JwtManager};
use crate::db::DatabaseRepo;
use crate::metrics::vm::VictoriaMetrics;

use super::handlers::{admin, auth, me, metrics};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<dyn DatabaseRepo>,
    pub jwt: Arc<JwtManager>,
    pub brute: Arc<BruteForceGuard>,
    pub vm: Arc<VictoriaMetrics>,
    pub allow_registration: bool,
    pub access_token_ttl_secs: u64,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Public
        .route("/health", get(auth::health))
        .route("/auth-method", post(auth::auth_method))
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .route("/token/refresh", post(auth::token_refresh))
        // PromQL proxy (requires JWT)
        .route("/api/v1/query", get(metrics::promql_query))
        .route("/api/v1/query_range", get(metrics::promql_query_range))
        // User self-service
        .route("/me/devices", get(me::me_devices))
        .route("/me/devices/:device_id", delete(me::me_delete_device))
        .route("/me/devices/:device_id/name", axum::routing::patch(me::me_rename_device))
        .route("/me/password", axum::routing::patch(me::me_change_password))
        // Admin
        .route("/admin/users", get(admin::admin_list_users).post(admin::admin_create_user))
        .route("/admin/users/:user_id", delete(admin::admin_delete_user))
        .route("/admin/users/:user_id/password", axum::routing::patch(admin::admin_change_user_password))
        .route("/admin/devices", get(admin::admin_list_devices))
        .route("/admin/devices/:device_id", delete(admin::admin_delete_device))
        .with_state(state)
}

// ─── Client IP extraction ───────────────────────────────────────────────────

/// Extract the real client IP from X-Forwarded-For header (last entry, proxy-added),
/// falling back to the direct connection address.
pub fn extract_client_ip(headers: &HeaderMap, addr: &SocketAddr) -> String {
    headers.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').last())  // last entry = proxy-added
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| addr.ip().to_string())
}

// ─── JWT extraction helper ───────────────────────────────────────────────────

/// Extract and verify the Bearer JWT from the Authorization header.
pub fn extract_jwt(headers: &HeaderMap, jwt: &JwtManager) -> Result<crate::auth::jwt::Claims, AppError> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::unauthorized("missing Authorization header"))?;

    let token = auth.strip_prefix("Bearer ")
        .ok_or_else(|| AppError::unauthorized("expected Bearer token"))?;

    jwt.verify_access(token)
        .map_err(|_| AppError::unauthorized("invalid or expired token"))
}

pub async fn require_admin(headers: &HeaderMap, jwt: &JwtManager, db: &dyn DatabaseRepo) -> Result<String, AppError> {
    let claims = extract_jwt(headers, jwt)?;
    let is_admin = db.user_is_admin(&claims.sub).await.map_err(AppError::internal)?;
    if is_admin {
        Ok(claims.sub)
    } else {
        Err(AppError::forbidden("admin role required"))
    }
}

// ─── Error type ─────────────────────────────────────────────────────────────

pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: "internal server error".to_string() }
    }
    pub fn unauthorized(msg: &str) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: msg.to_string() }
    }
    pub fn forbidden(msg: &str) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: msg.to_string() }
    }
    pub fn not_found(msg: &str) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: msg.to_string() }
    }
    pub fn conflict(msg: &str) -> Self {
        Self { status: StatusCode::CONFLICT, message: msg.to_string() }
    }
    pub fn locked_out(retry_after: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("too many attempts, retry after {retry_after}s"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}
