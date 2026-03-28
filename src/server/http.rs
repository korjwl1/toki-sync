use axum::{
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::{BruteForceGuard, JwtManager};
use crate::db::Database;
use crate::metrics::backend::MetricsBackend;
use crate::metrics::vm::VictoriaMetrics;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub jwt: Arc<JwtManager>,
    pub brute: Arc<BruteForceGuard>,
    pub vm: Arc<VictoriaMetrics>,
    pub allow_registration: bool,
    pub access_token_ttl_secs: u64,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Public
        .route("/health", get(health))
        .route("/auth-method", post(auth_method))
        .route("/login", post(login))
        .route("/register", post(register))
        .route("/token/refresh", post(token_refresh))
        // PromQL proxy (requires JWT)
        .route("/api/v1/query", get(promql_query))
        .route("/api/v1/query_range", get(promql_query_range))
        // User self-service
        .route("/me/devices", get(me_devices))
        .route("/me/devices/:device_id", delete(me_delete_device))
        .route("/me/devices/:device_id/name", axum::routing::patch(me_rename_device))
        .route("/me/password", axum::routing::patch(me_change_password))
        // Admin
        .route("/admin/users", get(admin_list_users).post(admin_create_user))
        .route("/admin/users/:user_id", delete(admin_delete_user))
        .route("/admin/users/:user_id/password", axum::routing::patch(admin_change_user_password))
        .route("/admin/devices", get(admin_list_devices))
        .route("/admin/devices/:device_id", delete(admin_delete_device))
        .with_state(state)
}

// ─── Client IP extraction ───────────────────────────────────────────────────

/// Extract the real client IP from X-Forwarded-For header (first entry),
/// falling back to the direct connection address.
fn extract_client_ip(headers: &HeaderMap, addr: &SocketAddr) -> String {
    headers.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| addr.ip().to_string())
}

// ─── JWT extraction helper ───────────────────────────────────────────────────

/// Extract and verify the Bearer JWT from the Authorization header.
fn extract_jwt(headers: &HeaderMap, jwt: &JwtManager) -> Result<crate::auth::jwt::Claims, AppError> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::unauthorized("missing Authorization header"))?;

    let token = auth.strip_prefix("Bearer ")
        .ok_or_else(|| AppError::unauthorized("expected Bearer token"))?;

    jwt.verify_access(token)
        .map_err(|_| AppError::unauthorized("invalid or expired token"))
}

async fn require_admin<'a>(headers: &'a HeaderMap, jwt: &'a JwtManager, db: &'a Database) -> Result<String, AppError> {
    let claims = extract_jwt(headers, jwt)?;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?")
        .bind(&claims.sub)
        .fetch_optional(&db.pool)
        .await
        .map_err(AppError::internal)?;
    match role.as_deref() {
        Some("admin") => Ok(claims.sub),
        _ => Err(AppError::forbidden("admin role required")),
    }
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

async fn auth_method(Json(body): Json<AuthMethodRequest>) -> impl IntoResponse {
    let _ = body.username;
    Json(serde_json::json!({ "method": "password" }))
}

// ─── /login ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    device_id: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: u64,
}

async fn login(
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

// ─── /token/refresh ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RefreshRequest {
    refresh_token: String,
    device_id: Option<String>,
}

async fn token_refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    let (access, refresh) = state.jwt
        .rotate(&state.db, &body.refresh_token, body.device_id.as_deref())
        .await
        .map_err(|_| AppError::unauthorized("invalid or expired refresh token"))?;

    Ok(Json(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_string(),
        expires_in: state.access_token_ttl_secs,
    }))
}

// ─── PromQL proxy ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct QueryParams {
    query: String,
    time: Option<i64>,
}

#[derive(Deserialize)]
struct QueryRangeParams {
    query: String,
    start: i64,
    end: i64,
    step: Option<String>,
}

async fn promql_query(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let injected = inject_user_label(&params.query, &claims.sub);
    let result = state.vm.query(&injected, params.time).await.map_err(AppError::internal)?;
    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
}

async fn promql_query_range(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<QueryRangeParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let injected = inject_user_label(&params.query, &claims.sub);
    let step = params.step.as_deref().unwrap_or("60s");
    let result = state.vm.query_range(&injected, params.start, params.end, step)
        .await
        .map_err(AppError::internal)?;
    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
}

// ─── Label injection ─────────────────────────────────────────────────────────

/// Escape a PromQL label value: backslash and double-quote must be escaped.
pub fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"'  => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c    => out.push(c),
        }
    }
    out
}

/// Inject `user="<escaped>"` into a PromQL expression by rewriting the
/// metric selector. This prevents a user from querying another user's data.
///
/// Strategy: find the first `{` in each metric selector and insert the label
/// before the first existing label. If no `{` exists, find each bare metric
/// name token and inject `{user="..."}` after it.
///
/// NOTE: This is a string-level transformation, not a full PromQL parse.
/// Handles the common patterns used by toki-monitor and toki CLI:
///   - bare metric:              `toki_tokens_total`
///   - aggregation argument:     `sum(toki_tokens_total)`
///   - range vector:             `rate(toki_tokens_total[5m])`
///   - aggregation with groupby: `sum by (model)(toki_tokens_total)`
///
/// All injected values are escaped; no string interpolation from user input.
pub fn inject_user_label(expr: &str, user_id: &str) -> String {
    let escaped = escape_label_value(user_id);
    let injection = format!("user=\"{escaped}\"");
    let selector = format!("{{{injection}}}");

    // ── Path A: expression already has `{...}` selectors ──────────────────
    if expr.contains('{') {
        let mut result = String::with_capacity(expr.len() + injection.len() + 10);
        let mut chars = expr.chars().peekable();

        while let Some(&ch) = chars.peek() {
            // Skip string literals
            if ch == '"' || ch == '\'' || ch == '`' {
                let quote = ch;
                result.push(chars.next().unwrap());
                while let Some(&c) = chars.peek() {
                    result.push(chars.next().unwrap());
                    if c == '\\' {
                        if let Some(&_next) = chars.peek() {
                            result.push(chars.next().unwrap());
                        }
                    } else if c == quote {
                        break;
                    }
                }
                continue;
            }
            // Inject into `{` label selectors
            if ch == '{' {
                result.push(chars.next().unwrap());
                if chars.peek() == Some(&'}') {
                    result.push_str(&injection);
                } else {
                    result.push_str(&injection);
                    result.push(',');
                }
                continue;
            }
            result.push(chars.next().unwrap());
        }
        return result;
    }

    // ── Path B: no `{` — inject after bare metric name tokens ─────────────
    //
    // A metric name token is `[a-zA-Z_:][a-zA-Z0-9_:]*` that is:
    //   - NOT immediately followed by `(` (which would make it a function name)
    //   - NOT a PromQL keyword (aggregation modifier, binary operator, etc.)
    //   - NOT inside an aggregation-modifier label list `by(...)` / `without(...)`
    //
    // We track parenthesis depth and whether we're inside a modifier label list.
    // Modifier label lists are `(...)` that directly follow `by`, `without`,
    // `on`, or `ignoring` keywords.

    const KEYWORDS: &[&str] = &[
        // aggregation operators (always followed by `(` or modifier keyword)
        "sum", "min", "max", "avg", "count", "stddev", "stdvar",
        "bottomk", "topk", "count_values", "quantile",
        // range/instant functions
        "rate", "irate", "increase", "delta", "idelta",
        "resets", "changes", "deriv", "predict_linear", "holt_winters",
        "label_replace", "label_join", "histogram_quantile",
        "abs", "absent", "ceil", "floor", "round", "clamp_max", "clamp_min",
        "exp", "sqrt", "ln", "log2", "log10",
        "vector", "scalar", "sort", "sort_desc",
        "time", "minute", "hour", "day_of_month", "day_of_week", "month", "year",
        // modifier keywords — their `(label, ...)` list must not get injected
        "by", "without", "on", "ignoring", "group_left", "group_right",
        // binary-operator keywords
        "and", "or", "unless", "bool", "offset",
    ];

    // Pre-scan: find byte ranges that are inside modifier-label lists.
    // A modifier-label list is `(...)` that immediately follows (with optional
    // whitespace) one of: `by`, `without`, `on`, `ignoring`.
    let bytes = expr.as_bytes();
    let len = bytes.len();
    let mut skip_range: Vec<(usize, usize)> = Vec::new();
    {
        let modifier_kw: &[&str] = &["by", "without", "on", "ignoring"];
        let mut i = 0;
        while i < len {
            if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let tok = &expr[start..i];
                if modifier_kw.contains(&tok) {
                    // skip whitespace
                    let mut j = i;
                    while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'(' {
                        // find matching ')'
                        let open = j;
                        let mut depth = 1usize;
                        let mut k = j + 1;
                        while k < len && depth > 0 {
                            if bytes[k] == b'(' { depth += 1; }
                            else if bytes[k] == b')' { depth -= 1; }
                            k += 1;
                        }
                        skip_range.push((open, k)); // k points past ')'
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    let in_skip_range = |pos: usize| skip_range.iter().any(|&(a, b)| pos >= a && pos < b);

    let mut result = String::with_capacity(expr.len() + selector.len() * 2);
    let mut i = 0;
    let mut bracket_depth = 0u32; // depth inside `[...]` range vectors
    while i < len {
        let b = bytes[i];
        if b == b'[' {
            bracket_depth += 1;
            result.push(b as char);
            i += 1;
        } else if b == b']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            result.push(b as char);
            i += 1;
        } else if b.is_ascii_alphabetic() || b == b'_' || b == b':' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b':') {
                i += 1;
            }
            let tok = &expr[start..i];
            result.push_str(tok);

            // Never inject inside `[...]` (duration suffixes like `5m`, `1h`)
            if bracket_depth == 0 {
                let next = bytes.get(i).copied();
                let is_fn   = next == Some(b'(');
                let is_kw   = KEYWORDS.contains(&tok);
                let in_skip = in_skip_range(start);

                if !is_fn && !is_kw && !in_skip {
                    result.push_str(&selector);
                }
            }
        } else {
            result.push(b as char);
            i += 1;
        }
    }

    result
}

// ─── /register ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    password: String,
}

async fn register(
    ConnectInfo(_addr): ConnectInfo<SocketAddr>,
    _headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
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

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at) VALUES (?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&body.username)
    .bind(&hash)
    .bind("user")
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("username already exists")
        } else {
            AppError::internal(e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id, "username": body.username }))))
}

// ─── /me endpoints ──────────────────────────────────────────────────────────

async fn me_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id, name, last_seen_at FROM devices WHERE user_id = ? ORDER BY last_seen_at DESC",
    )
    .bind(&claims.sub)
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|(id, name, last_seen)| {
        serde_json::json!({ "id": id, "name": name, "last_seen_at": last_seen })
    }).collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

#[derive(Deserialize)]
struct RenameDeviceRequest {
    name: String,
}

async fn me_rename_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
    Json(body): Json<RenameDeviceRequest>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "name must not be empty".into() });
    }

    let affected = sqlx::query(
        "UPDATE devices SET name = ? WHERE id = ? AND user_id = ?",
    )
    .bind(&name)
    .bind(&device_id)
    .bind(&claims.sub)
    .execute(&state.db.pool)
    .await
    .map_err(AppError::internal)?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me_delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    // Verify ownership first
    let exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM devices WHERE id = ? AND user_id = ?"
    )
    .bind(&device_id)
    .bind(&claims.sub)
    .fetch_one(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    if !exists {
        return Err(AppError::not_found("device not found"));
    }

    // Ownership confirmed — safe to delete VM data
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    sqlx::query("DELETE FROM devices WHERE id = ? AND user_id = ?")
        .bind(&device_id)
        .bind(&claims.sub)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

async fn me_change_password(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?")
        .bind(&claims.sub)
        .fetch_optional(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    let (hash,) = row.ok_or_else(|| AppError::not_found("user not found"))?;

    let cur = body.current_password.clone();
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(&cur, &hash))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    if !valid {
        return Err(AppError::unauthorized("current password incorrect"));
    }

    if body.new_password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let new_pw = body.new_password.clone();
    let new_hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(&new_pw, bcrypt::DEFAULT_COST)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(AppError::internal)?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(now)
        .bind(&claims.sub)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

// ─── /admin endpoints ───────────────────────────────────────────────────────

async fn admin_list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT id, username, role, created_at FROM users ORDER BY created_at",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let users: Vec<_> = rows.into_iter().map(|(id, username, role, created_at)| {
        serde_json::json!({ "id": id, "username": username, "role": role, "created_at": created_at })
    }).collect();

    Ok(Json(serde_json::json!({ "users": users })))
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
    role: Option<String>,
}

async fn admin_create_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

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
    let role = body.role.as_deref().unwrap_or("user");
    if role != "user" && role != "admin" {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "role must be 'user' or 'admin'".into(),
        });
    }

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at) VALUES (?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&body.username)
    .bind(&hash)
    .bind(role)
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("username already exists")
        } else {
            AppError::internal(e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id, "username": body.username, "role": role }))))
}

async fn admin_delete_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Delete VM series for all user's devices before cascade
    let device_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM devices WHERE user_id = ?"
    )
    .bind(&user_id)
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    for did in &device_ids {
        if let Err(e) = state.vm.delete_device_series(did).await {
            tracing::warn!("failed to delete VM series for device {did}: {e}");
        }
    }

    let affected = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("user not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AdminChangePasswordRequest {
    password: String,
}

async fn admin_change_user_password(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
    Json(body): Json<AdminChangePasswordRequest>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Verify user exists
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_optional(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    if exists.is_none() {
        return Err(AppError::not_found("user not found"));
    }

    if body.password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let pw = body.password.clone();
    let new_hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(now)
        .bind(&user_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn admin_list_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT d.id, d.name, u.username, d.last_seen_at FROM devices d
         JOIN users u ON d.user_id = u.id ORDER BY d.last_seen_at DESC",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|(id, name, username, last_seen)| {
        serde_json::json!({ "id": id, "name": name, "username": username, "last_seen_at": last_seen })
    }).collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

async fn admin_delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Delete the device's time-series data from VictoriaMetrics before removing from DB
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    let affected = sqlx::query("DELETE FROM devices WHERE id = ?")
        .bind(&device_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ─── Error type ─────────────────────────────────────────────────────────────

pub struct AppError {
    status: StatusCode,
    message: String,
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

// ─── Label injection tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escape_label_value("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_escape_double_quote() {
        assert_eq!(escape_label_value("a\"b"), "a\\\"b");
    }

    #[test]
    fn test_escape_newline() {
        assert_eq!(escape_label_value("a\nb"), "a\\nb");
    }

    #[test]
    fn test_escape_no_special_chars() {
        assert_eq!(escape_label_value("user-123"), "user-123");
    }

    #[test]
    fn test_inject_bare_metric() {
        // No selector → append {user="..."}
        let result = inject_user_label("up", "alice");
        assert!(result.contains("user=\"alice\""), "got: {result}");
        assert!(result.starts_with("up{"), "got: {result}");
    }

    #[test]
    fn test_inject_empty_selector() {
        let result = inject_user_label("toki_usage{}", "bob");
        assert!(result.contains("user=\"bob\""), "got: {result}");
        assert!(!result.contains(",}"), "should not have trailing comma: {result}");
    }

    #[test]
    fn test_inject_existing_labels() {
        let result = inject_user_label("toki_usage{model=\"gpt-4\"}", "carol");
        assert!(result.contains("user=\"carol\""), "got: {result}");
        assert!(result.contains("model=\"gpt-4\""), "existing label preserved: {result}");
    }

    #[test]
    fn test_injection_attempt_escaped() {
        // Attacker tries to inject: attacker",user="victim
        // After escape: attacker\",user=\"victim
        // This is safe: PromQL treats \" as escaped quote inside the string,
        // so there is no separate user="victim" label.
        let malicious_user = "attacker\",user=\"victim";
        let result = inject_user_label("metric{}", malicious_user);
        // Escaped quote must appear — the " is not raw in output
        assert!(result.contains("\\\""), "quote must be escaped, got: {result}");
        // Must NOT contain an unescaped standalone label like ,user="victim"
        // i.e. after the escaped quote there must not be a bare: ,user="
        assert!(!result.contains(",user=\"victim\""), "standalone injection label must not appear, got: {result}");
    }

    #[test]
    fn test_injection_backslash_escaped() {
        let user = "user\\admin";
        let result = inject_user_label("m{}", user);
        assert!(result.contains("user\\\\admin"), "backslash must be doubled, got: {result}");
    }

    #[test]
    fn test_inject_range_query() {
        // rate(metric[5m]) — bare metric inside function, label before `[`
        let result = inject_user_label("rate(toki_tokens_total[5m])", "dave");
        assert_eq!(result, "rate(toki_tokens_total{user=\"dave\"}[5m])", "got: {result}");
    }

    #[test]
    fn test_inject_aggregation_no_selector() {
        // sum(metric) — metric inside aggregation, no existing selector
        let result = inject_user_label("sum(toki_tokens_total)", "eve");
        assert_eq!(result, "sum(toki_tokens_total{user=\"eve\"})", "got: {result}");
    }

    #[test]
    fn test_inject_aggregation_with_by() {
        // sum by (model)(metric) — `model` inside `by(...)` must NOT get injected
        let result = inject_user_label("sum by (model)(toki_tokens_total)", "frank");
        assert!(result.contains("toki_tokens_total{user=\"frank\"}"), "metric must be injected, got: {result}");
        // `model` inside `by(...)` must not get a selector appended
        assert!(!result.contains("model{"), "label in by() must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_increase_range() {
        // increase(metric[1h]) — similar to rate
        let result = inject_user_label("increase(toki_tokens_total[1h])", "grace");
        assert_eq!(result, "increase(toki_tokens_total{user=\"grace\"}[1h])", "got: {result}");
    }

    #[test]
    fn test_inject_nested_sum_by_increase() {
        // toki-monitor primary pattern: sum by (model) (increase(metric[step]))
        let result = inject_user_label(
            "sum by (model) (increase(toki_tokens_total[15m]))", "hana");
        assert_eq!(
            result,
            "sum by (model) (increase(toki_tokens_total{user=\"hana\"}[15m]))",
            "got: {result}",
        );
        // model inside by(...) must NOT get a selector
        assert!(!result.contains("model{"), "label in by() must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_nested_sum_by_increase_with_filter() {
        // toki-monitor with provider filter: Path A (expr contains `{`)
        let result = inject_user_label(
            "sum by (model) (increase(toki_tokens_total{provider=\"claude_code\"}[1h]))", "ivan");
        assert!(result.contains("user=\"ivan\""), "got: {result}");
        assert!(result.contains("provider=\"claude_code\""), "existing label preserved, got: {result}");
        // injection must be inside the `{...}`, not appended at end
        assert!(result.contains("toki_tokens_total{"), "got: {result}");
        assert!(!result.contains("}[1h]){"), "selector must not appear after closing paren, got: {result}");
    }

    #[test]
    fn test_inject_multi_label_by() {
        // sum by (model, provider)(metric) — multi-label modifier list
        let result = inject_user_label(
            "sum by (model, provider) (increase(toki_tokens_total[1h]))", "judy");
        assert!(result.contains("toki_tokens_total{user=\"judy\"}"), "got: {result}");
        assert!(!result.contains("model{"), "model label must not be injected, got: {result}");
        assert!(!result.contains("provider{"), "provider label must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_without_clause() {
        // sum without (session)(metric)
        let result = inject_user_label(
            "sum without (session) (increase(toki_tokens_total[1h]))", "ken");
        assert!(result.contains("toki_tokens_total{user=\"ken\"}"), "got: {result}");
        assert!(!result.contains("session{"), "session label must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_trailing_by_syntax() {
        // sum(increase(metric[1h])) by (model) — alternative PromQL syntax (by after closing paren)
        let result = inject_user_label(
            "sum(increase(toki_tokens_total[1h])) by (model)", "leo");
        assert!(result.contains("toki_tokens_total{user=\"leo\"}"), "got: {result}");
        assert!(!result.contains("model{"), "model label must not be injected, got: {result}");
    }
}
