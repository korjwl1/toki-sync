use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::auth::{BruteForceGuard, JwtManager};
use crate::auth::oidc::{OidcDiscovery, OidcStateStore};
use crate::db::DatabaseRepo;
use crate::metrics::vm::VictoriaMetrics;

use super::handlers::{admin, auth, dashboard, me, metrics, teams};

/// Dynamic settings that check DB first, then fall back to config file values.
#[derive(Clone)]
pub struct DynamicSettings {
    pub db: Arc<dyn DatabaseRepo>,
    pub config_registration_mode: String,
    pub config_oidc_issuer: String,
    pub config_oidc_client_id: String,
    pub config_oidc_client_secret: String,
    pub config_oidc_redirect_uri: String,
}

impl DynamicSettings {
    pub async fn registration_mode(&self) -> String {
        self.db.get_server_setting("registration_mode").await
            .ok().flatten()
            .unwrap_or_else(|| self.config_registration_mode.clone())
    }
    pub async fn oidc_issuer(&self) -> String {
        self.db.get_server_setting("oidc_issuer").await
            .ok().flatten()
            .unwrap_or_else(|| self.config_oidc_issuer.clone())
    }
    pub async fn oidc_client_id(&self) -> String {
        self.db.get_server_setting("oidc_client_id").await
            .ok().flatten()
            .unwrap_or_else(|| self.config_oidc_client_id.clone())
    }
    pub async fn oidc_client_secret(&self) -> String {
        self.db.get_server_setting("oidc_client_secret").await
            .ok().flatten()
            .unwrap_or_else(|| self.config_oidc_client_secret.clone())
    }
    pub async fn oidc_redirect_uri(&self) -> String {
        self.db.get_server_setting("oidc_redirect_uri").await
            .ok().flatten()
            .unwrap_or_else(|| self.config_oidc_redirect_uri.clone())
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<dyn DatabaseRepo>,
    pub jwt: Arc<JwtManager>,
    pub brute: Arc<BruteForceGuard>,
    pub vm: Arc<VictoriaMetrics>,
    pub access_token_ttl_secs: u64,
    pub oidc_state_store: Arc<OidcStateStore>,
    /// Cached OIDC discovery result with TTL.
    pub oidc_discovery_cache: Arc<tokio::sync::RwLock<Option<(OidcDiscovery, Instant)>>>,
    /// Shared HTTP client for OIDC requests.
    pub oidc_http_client: reqwest::Client,
    /// External URL for JWT `iss` claim and OIDC redirect derivation.
    pub external_url: String,
    /// Storage backend name (e.g. "sqlite", "postgres") for server-info endpoint.
    pub storage_backend: String,
    /// Track last poll time per device_code to enforce slow_down (RFC 8628).
    pub device_poll_tracker: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    /// Dynamic settings: DB overrides + config fallback.
    pub dynamic_settings: DynamicSettings,
    /// Whether to trust X-Forwarded-For header for client IP extraction.
    /// Set to true when deployed behind a reverse proxy (e.g. Caddy, nginx).
    pub trust_proxy: bool,
}

pub async fn get_oidc_discovery(state: &AppState) -> Result<OidcDiscovery, AppError> {
    const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour

    // Check cache
    {
        let cache = state.oidc_discovery_cache.read().await;
        if let Some((ref disc, ref cached_at)) = *cache {
            if cached_at.elapsed() < CACHE_TTL {
                return Ok(disc.clone());
            }
        }
    }

    // Use dynamic issuer
    let issuer = state.dynamic_settings.oidc_issuer().await;

    // Fetch fresh
    let disc = crate::auth::oidc::discover(&issuer, &state.oidc_http_client)
        .await
        .map_err(AppError::internal)?;

    let mut cache = state.oidc_discovery_cache.write().await;
    *cache = Some((disc.clone(), Instant::now()));
    Ok(disc)
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Admin panel (public HTML pages)
        .route("/", get(dashboard::admin_redirect))
        .route("/admin", get(dashboard::admin_page))
        // Public
        .route("/health", get(auth::health))
        .route("/auth-method", post(auth::auth_method))
        .route("/login", get(dashboard::login_page).post(auth::login))
        .route("/register", post(auth::register))
        .route("/token/refresh", post(auth::token_refresh))
        // Auth info
        .route("/auth/info", get(auth::auth_info))
        // Device Authorization Grant (RFC 8628)
        .route("/device/code", post(auth::device_code_request))
        .route("/device/token", post(auth::device_token_poll))
        .route("/device/approve", post(auth::device_approve))
        .route("/login/device", get(auth::device_login_page))
        // OIDC (Phase 3)
        .route("/auth/oidc/authorize", get(auth::oidc_authorize))
        .route("/auth/callback", get(auth::oidc_callback))
        // PromQL proxy (requires JWT)
        .route("/api/v1/query", get(metrics::promql_query))
        .route("/api/v1/query_range", get(metrics::promql_query_range))
        // User self-service
        .route("/me/devices", get(me::me_devices))
        .route("/me/devices/:device_id", delete(me::me_delete_device))
        .route("/me/devices/:device_id/name", axum::routing::patch(me::me_rename_device))
        .route("/me/password", axum::routing::patch(me::me_change_password))
        .route("/me/teams", get(teams::me_teams))
        // Admin
        .route("/admin/users", get(admin::admin_list_users).post(admin::admin_create_user))
        .route("/admin/users/:user_id", delete(admin::admin_delete_user))
        .route("/admin/users/:user_id/password", axum::routing::patch(admin::admin_change_user_password))
        .route("/admin/users/:user_id/role", axum::routing::patch(admin::admin_change_user_role))
        .route("/admin/devices", get(admin::admin_list_devices))
        .route("/admin/devices/:device_id", delete(admin::admin_delete_device))
        // Admin: pending registrations
        .route("/admin/pending", get(admin::admin_list_pending))
        .route("/admin/pending/:id/approve", post(admin::admin_approve_pending))
        .route("/admin/pending/:id/reject", post(admin::admin_reject_pending))
        // Admin: server info & settings
        .route("/admin/server-info", get(admin::admin_server_info))
        .route("/admin/settings", get(admin::admin_list_settings))
        .route("/admin/settings/:key", axum::routing::put(admin::admin_update_setting))
        // Admin: user active status
        .route("/admin/users/:user_id/active", axum::routing::patch(admin::admin_set_user_active))
        // Admin: teams
        .route("/admin/teams", get(teams::admin_list_teams).post(teams::admin_create_team))
        .route("/admin/teams/:team_id", delete(teams::admin_delete_team))
        .route("/admin/teams/:team_id/members", get(teams::admin_list_team_members).post(teams::admin_add_team_member))
        .route("/admin/teams/:team_id/members/:user_id", delete(teams::admin_remove_team_member))
        // Team usage aggregation
        .route("/api/v1/teams/:team_id/query_range", get(teams::team_query_range))
        .with_state(state)
}

// ─── Shared validation helpers ──────────────────────────────────────────────

/// Validate a username: 3-32 chars, alphanumeric + `_`, `-`, `.` only.
pub fn validate_username(username: &str) -> Result<(), AppError> {
    if username.len() < 3 || username.len() > 32 {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "username must be 3-32 characters".into(),
        });
    }
    if !username.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "username may only contain letters, digits, _, -, .".into(),
        });
    }
    Ok(())
}

// ─── Client IP extraction ───────────────────────────────────────────────────

/// Extract the real client IP, optionally trusting X-Forwarded-For.
///
/// When `trust_proxy` is true (deployed behind a reverse proxy), the last entry
/// in X-Forwarded-For is used (the proxy-appended client IP).
/// When `trust_proxy` is false (default, direct connections), the header is
/// ignored entirely to prevent spoofing.
pub fn extract_client_ip(headers: &HeaderMap, addr: &SocketAddr, trust_proxy: bool) -> String {
    if trust_proxy {
        headers.get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').last())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| addr.ip().to_string())
    } else {
        addr.ip().to_string()
    }
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
    pub fn bad_gateway(e: impl std::fmt::Display) -> Self {
        tracing::error!("backend unavailable: {e}");
        Self { status: StatusCode::BAD_GATEWAY, message: "metrics backend unavailable".to_string() }
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
