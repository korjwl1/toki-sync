//! Monitor settings sync: an opt-in, user-scoped key/value channel for
//! toki_monitor's own configuration and dashboard definitions.
//!
//! This is deliberately NOT part of `toki_sync_protocol`. That protocol carries
//! toki's collected usage data and only that; monitor configuration rides here
//! instead, over plain HTTP with the same auth as every other `/me` route. The
//! separation is the requirement: a monitor user who wants their dashboards on
//! more than one machine opts into this, and a toki user who does not run the
//! monitor never touches it.
//!
//! Payloads are opaque. The server stores the bytes the monitor sends and hands
//! the same bytes back — it never parses a dashboard, so there is no second
//! server-side idea of what a dashboard is to drift out of step with the real
//! one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::db::models::{MonitorDeleteOutcome, MonitorQuota, MonitorWriteOutcome};

use super::super::http::{authenticate, AppError, AppState};

// ─── Limits ─────────────────────────────────────────────────────────────────

/// Largest single payload. A dashboard definition is a few KB; this leaves room
/// for a big one without letting a client park a video in the settings store.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;

/// Largest key. Keys are namespaced by the monitor (`dashboard:<id>`,
/// `prefs:theme`, …), not free-form user text.
pub const MAX_KEY_LEN: usize = 128;

/// Per-user entry count ceiling.
pub const MAX_ENTRIES: i64 = 512;

/// Per-user byte ceiling across all entries.
pub const MAX_TOTAL_BYTES: i64 = 8 * 1024 * 1024;

/// Body limit on the write route, enforced by axum BEFORE the body is buffered.
/// Without it a 50 MB PUT would be read into memory in full and only then
/// rejected for exceeding [`MAX_VALUE_BYTES`]. Sized to admit a maximal value
/// after JSON escaping (worst case ~6x for control characters) plus the key.
pub const MAX_REQUEST_BODY: usize = 2 * 1024 * 1024;

pub fn quota() -> MonitorQuota {
    MonitorQuota { max_entries: MAX_ENTRIES, max_total_bytes: MAX_TOTAL_BYTES }
}

// ─── Rate limiting ──────────────────────────────────────────────────────────

/// Per-user write budget for this channel.
///
/// Same mechanism as the sync path's `WindowsRateLimiter`: one shared
/// `Mutex<HashMap>` keyed by identity, swept opportunistically so it cannot
/// grow with every user the process has ever seen. One deliberate difference —
/// the sync limiter enforces a MINIMUM INTERVAL between batches, and that rule
/// is wrong here. A monitor saving its configuration writes several entries
/// back to back (each edited dashboard, the panel layout, preferences), and an
/// interval floor would accept the first and refuse the rest. So this counts
/// writes inside a fixed window instead: a burst passes, a loop does not.
pub type MonitorWriteRateLimiter = Arc<MonitorWriteRateLimiterInner>;

#[derive(Default)]
pub struct MonitorWriteRateLimiterInner {
    /// user_id -> (window start, writes so far in that window)
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl MonitorWriteRateLimiterInner {
    const WINDOW: Duration = Duration::from_secs(60);
    const MAX_WRITES: u32 = 60;
    const SWEEP_AT: usize = 4096;

    pub fn new() -> Self {
        Self::default()
    }

    /// `Ok(())` if the write may proceed, `Err(retry_after_secs)` if not.
    pub fn allow(&self, user_id: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());

        if map.len() >= Self::SWEEP_AT {
            map.retain(|_, (start, _)| now.duration_since(*start) < Self::WINDOW);
        }

        let entry = map.entry(user_id.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= Self::WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= Self::MAX_WRITES {
            let elapsed = now.duration_since(entry.0);
            return Err(Self::WINDOW.saturating_sub(elapsed).as_secs().max(1));
        }
        entry.1 += 1;
        Ok(())
    }
}

/// 429 carrying `Retry-After`, so a client backs off by the server's clock
/// instead of guessing.
fn too_many_writes(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", retry_after.to_string())],
        Json(serde_json::json!({
            "error": format!("too many monitor setting writes, retry after {retry_after}s"),
            "retry_after": retry_after,
        })),
    )
        .into_response()
}

// ─── Key validation ─────────────────────────────────────────────────────────

/// Keys are an identifier namespace, not free text: ASCII alphanumerics plus
/// `. _ - :`. No slashes (they would not survive the route), no control
/// characters, no Unicode lookalikes that would let two entries print the same.
fn validate_key(key: &str) -> Result<(), AppError> {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: format!("key must be 1-{MAX_KEY_LEN} characters"),
        });
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
    {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "key may only contain letters, digits, and . _ - :".into(),
        });
    }
    Ok(())
}

// ─── GET /me/monitor/index ──────────────────────────────────────────────────

/// What this user has stored, WITHOUT the payloads, plus quota headroom. A
/// client deciding what is worth fetching compares `version` against what it
/// already holds and pulls only the entries that moved.
pub async fn me_monitor_index(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = authenticate(&state, &headers).await?;

    let rows = state
        .db
        .list_monitor_setting_index(&claims.sub)
        .await
        .map_err(AppError::internal)?;
    let usage = state
        .db
        .monitor_settings_usage(&claims.sub)
        .await
        .map_err(AppError::internal)?;

    let entries: Vec<_> = rows
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "key": m.key,
                "version": m.version,
                "updated_at": m.updated_at,
                "size_bytes": m.size_bytes,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        // Capability gate for older clients/servers. A client must not send a
        // conditional delete to a server that may ignore the query parameter.
        "delete_cas": true,
        "entries": entries,
        "quota": {
            "max_entries": MAX_ENTRIES,
            "max_value_bytes": MAX_VALUE_BYTES,
            "max_total_bytes": MAX_TOTAL_BYTES,
            "used_entries": usage.entries,
            "used_bytes": usage.total_bytes,
        }
    })))
}

// ─── GET /me/monitor/settings ───────────────────────────────────────────────

/// Everything this user has stored, payloads included.
pub async fn me_monitor_list(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = authenticate(&state, &headers).await?;

    let rows = state
        .db
        .list_monitor_settings(&claims.sub)
        .await
        .map_err(AppError::internal)?;

    let entries: Vec<_> = rows
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "key": e.key,
                "value": e.value,
                "version": e.version,
                "updated_at": e.updated_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "entries": entries })))
}

// ─── GET /me/monitor/settings/{key} ─────────────────────────────────────────

pub async fn me_monitor_get(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = authenticate(&state, &headers).await?;
    validate_key(&key)?;

    let entry = state
        .db
        .get_monitor_setting(&claims.sub, &key)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("monitor setting not found"))?;

    Ok(Json(serde_json::json!({
        "key": entry.key,
        "value": entry.value,
        "version": entry.version,
        "updated_at": entry.updated_at,
    })))
}

// ─── PUT /me/monitor/settings/{key} ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct PutSettingRequest {
    /// Opaque payload. A string, not a JSON value, on purpose: the monitor
    /// stringifies its own structure and gets the identical bytes back, so the
    /// server never re-serialises (and therefore never reorders keys or
    /// normalises numbers in) somebody else's document.
    pub value: String,
    /// Optional compare-and-swap. When present the write only lands if the
    /// stored version matches; `0` means "expect no entry" (create-only).
    #[serde(default)]
    pub if_version: Option<i64>,
}

pub async fn me_monitor_put(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<PutSettingRequest>,
) -> Result<Response, AppError> {
    let claims = authenticate(&state, &headers).await?;
    validate_key(&key)?;

    if body.value.len() > MAX_VALUE_BYTES {
        return Err(AppError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: format!(
                "value is {} bytes, limit is {MAX_VALUE_BYTES}",
                body.value.len()
            ),
        });
    }

    if let Err(retry_after) = state.monitor_rate.allow(&claims.sub) {
        return Ok(too_many_writes(retry_after));
    }

    let outcome = state
        .db
        .upsert_monitor_setting(&claims.sub, &key, &body.value, body.if_version, quota())
        .await
        .map_err(AppError::internal)?;

    Ok(match outcome {
        MonitorWriteOutcome::Written { version, updated_at, previous_version } => (
            StatusCode::OK,
            Json(serde_json::json!({
                "key": key,
                "version": version,
                "updated_at": updated_at,
                // What this write replaced. A client that wrote without
                // `if_version` compares it against the version it had fetched:
                // if they differ, another device wrote in between and this
                // write just overwrote it.
                "previous_version": previous_version,
                "created": previous_version.is_none(),
            })),
        )
            .into_response(),

        MonitorWriteOutcome::VersionMismatch { current_version, current_updated_at } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "version conflict: the stored entry moved since if_version was read",
                "key": key,
                "current_version": current_version,
                "current_updated_at": current_updated_at,
            })),
        )
            .into_response(),

        MonitorWriteOutcome::QuotaExceeded { what, used, limit } => (
            StatusCode::INSUFFICIENT_STORAGE,
            Json(serde_json::json!({
                "error": format!("monitor settings quota exceeded: {what} would reach {used}, limit is {limit}"),
                "quota": what,
                "used": used,
                "limit": limit,
            })),
        )
            .into_response(),
    })
}

// ─── DELETE /me/monitor/settings/{key} ──────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteSettingRequest {
    #[serde(default)]
    pub if_version: Option<i64>,
}

pub async fn me_monitor_delete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<DeleteSettingRequest>,
) -> Result<Response, AppError> {
    let claims = authenticate(&state, &headers).await?;
    validate_key(&key)?;

    if let Err(retry_after) = state.monitor_rate.allow(&claims.sub) {
        return Ok(too_many_writes(retry_after));
    }

    let outcome = state
        .db
        .delete_monitor_setting(&claims.sub, &key, query.if_version)
        .await
        .map_err(AppError::internal)?;

    Ok(match outcome {
        MonitorDeleteOutcome::Deleted => StatusCode::NO_CONTENT.into_response(),
        MonitorDeleteOutcome::NotFound => {
            return Err(AppError::not_found("monitor setting not found"));
        }
        MonitorDeleteOutcome::VersionMismatch { current_version, current_updated_at } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "version conflict: the stored entry moved since if_version was read",
                "key": key,
                "current_version": current_version,
                "current_updated_at": current_updated_at,
            })),
        )
            .into_response(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{BruteForceGuard, JwtManager};
    use crate::db::models::{MonitorQuota, NewUser};
    use crate::db::sqlite::SqliteRepo;
    use crate::db::DatabaseRepo;
    use crate::server::http::{build_router, ActiveCacheInner, DynamicSettings};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// The real router, backed by a real SQLite file and real JWTs. Calling the
    /// handler functions directly would prove nothing about isolation: the
    /// point is that the user id comes from a verified token and from nowhere
    /// else, and that only shows up if routing and auth actually run.
    struct Harness {
        router: axum::Router,
        jwt: Arc<JwtManager>,
        db: Arc<SqliteRepo>,
        _db_file: tempfile::NamedTempFile,
        _events_dir: tempfile::TempDir,
    }

    impl Harness {
        async fn new(users: &[&str]) -> Self {
            let db_file = tempfile::NamedTempFile::new().unwrap();
            let events_dir = tempfile::tempdir().unwrap();

            let db = Arc::new(SqliteRepo::open(db_file.path().to_str().unwrap()).await.unwrap());
            for u in users {
                db.create_user(&NewUser {
                    id: (*u).to_string(),
                    username: (*u).to_string(),
                    password_hash: "x".into(),
                    role: "user".into(),
                })
                .await
                .unwrap();
            }

            let db_dyn: Arc<dyn DatabaseRepo> = db.clone();
            let jwt = Arc::new(JwtManager::new("test-secret", 3600, 86400));
            let events: Arc<dyn crate::events::EventStore> = Arc::new(
                crate::events::fjall_store::FjallEventStore::open(events_dir.path()).unwrap(),
            );

            let state = crate::server::http::AppState {
                db: db_dyn.clone(),
                jwt: jwt.clone(),
                brute: Arc::new(BruteForceGuard::new(5, 300, 900)),
                events,
                access_token_ttl_secs: 3600,
                oidc_state_store: Arc::new(crate::auth::oidc::OidcStateStore::new(600)),
                oidc_discovery_cache: Arc::new(tokio::sync::RwLock::new(None)),
                oidc_http_client: reqwest::Client::new(),
                external_url: String::new(),
                storage_backend: "sqlite".into(),
                device_poll_tracker: Arc::new(Mutex::new(HashMap::new())),
                dynamic_settings: DynamicSettings {
                    db: db_dyn,
                    config_registration_mode: "closed".into(),
                    config_oidc_issuer: String::new(),
                    config_oidc_client_id: String::new(),
                    config_oidc_client_secret: String::new(),
                    config_oidc_redirect_uri: String::new(),
                    config_max_query_scope: "30d".into(),
                },
                trust_proxy: false,
                pricing: Arc::new(tokio::sync::RwLock::new(
                    crate::pricing::PricingTable::new(HashMap::new()),
                )),
                active_cache: Arc::new(ActiveCacheInner::new()),
                monitor_rate: Arc::new(MonitorWriteRateLimiterInner::new()),
            };

            Self {
                router: build_router(state),
                jwt,
                db,
                _db_file: db_file,
                _events_dir: events_dir,
            }
        }

        fn token(&self, user: &str) -> String {
            self.jwt.issue_access_token(user).unwrap()
        }

        async fn send(&self, req: Request<Body>) -> (StatusCode, serde_json::Value) {
            let resp = self.router.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, json)
        }

        async fn get(&self, path: &str, user: &str) -> (StatusCode, serde_json::Value) {
            self.send(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .header("authorization", format!("Bearer {}", self.token(user)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        async fn put(
            &self,
            path: &str,
            user: &str,
            body: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            self.send(
                Request::builder()
                    .method("PUT")
                    .uri(path)
                    .header("authorization", format!("Bearer {}", self.token(user)))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
        }

        async fn delete(&self, path: &str, user: &str) -> (StatusCode, serde_json::Value) {
            self.send(
                Request::builder()
                    .method("DELETE")
                    .uri(path)
                    .header("authorization", format!("Bearer {}", self.token(user)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }
    }

    // ── Per-user isolation ─────────────────────────────────────────────────

    /// Two accounts, the SAME key. Neither may see, overwrite, or delete the
    /// other's entry, and neither may learn the other's key exists.
    #[tokio::test]
    async fn entries_are_isolated_per_user() {
        let h = Harness::new(&["alice", "bob"]).await;

        h.put("/me/monitor/settings/dash:main", "alice", serde_json::json!({ "value": "alice-payload" })).await;
        h.put("/me/monitor/settings/dash:main", "bob", serde_json::json!({ "value": "bob-payload" })).await;
        h.put("/me/monitor/settings/alice.only", "alice", serde_json::json!({ "value": "secret" })).await;

        // Reads do not cross.
        let (s, v) = h.get("/me/monitor/settings/dash:main", "alice").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["value"], "alice-payload");
        let (s, v) = h.get("/me/monitor/settings/dash:main", "bob").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["value"], "bob-payload");

        // Both wrote the same key and both got version 1: bob's write created
        // his own row rather than bumping alice's.
        assert_eq!(v["version"], 1);

        // A key only alice has is a 404 for bob, not a 403 -- he learns nothing.
        let (s, _) = h.get("/me/monitor/settings/alice.only", "bob").await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // Listings are scoped.
        let (_, v) = h.get("/me/monitor/index", "bob").await;
        let keys: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["dash:main"], "bob's index must not name alice's keys");
        assert_eq!(v["quota"]["used_entries"], 1);

        let (_, v) = h.get("/me/monitor/settings", "alice").await;
        assert_eq!(v["entries"].as_array().unwrap().len(), 2);

        // Bob cannot CAS onto alice's row. Alice's `alice.only` is at version 1,
        // so an if_version=1 write would land if the CAS were reading her row.
        // It is not: bob's own namespace has no such key, so the comparison is
        // against nothing and the write is refused.
        let (s, v) = h.put(
            "/me/monitor/settings/alice.only",
            "bob",
            serde_json::json!({ "value": "hijack", "if_version": 1 }),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "bob must not CAS onto a key he does not have");
        assert!(v["current_version"].is_null());
        // Alice's entry is untouched.
        let (_, v) = h.get("/me/monitor/settings/alice.only", "alice").await;
        assert_eq!(v["value"], "secret");

        // Bob deleting the shared key removes only his own row.
        let (s, _) = h.delete("/me/monitor/settings/dash:main", "bob").await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, v) = h.get("/me/monitor/settings/dash:main", "alice").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["value"], "alice-payload");

        // Bob deleting a key only alice has finds nothing, and alice keeps it.
        let (s, _) = h.delete("/me/monitor/settings/alice.only", "bob").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert!(h.db.get_monitor_setting("alice", "alice.only").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unauthenticated_requests_are_refused() {
        let h = Harness::new(&["alice"]).await;
        for (method, uri) in [
            ("GET", "/me/monitor/index"),
            ("GET", "/me/monitor/settings"),
            ("GET", "/me/monitor/settings/k"),
            ("PUT", "/me/monitor/settings/k"),
            ("DELETE", "/me/monitor/settings/k"),
        ] {
            let (s, _) = h
                .send(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"value":"x"}"#))
                        .unwrap(),
                )
                .await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    /// A forged token signed with the wrong secret must not read anything.
    #[tokio::test]
    async fn a_token_from_another_signer_is_refused() {
        let h = Harness::new(&["alice"]).await;
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "v" })).await;

        let forged = JwtManager::new("not-the-secret", 3600, 86400)
            .issue_access_token("alice")
            .unwrap();
        let (s, _) = h
            .send(
                Request::builder()
                    .method("GET")
                    .uri("/me/monitor/settings/k")
                    .header("authorization", format!("Bearer {forged}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    // ── Round-trip fidelity ────────────────────────────────────────────────

    /// The server must hand back exactly the bytes it was given. If it ever
    /// parsed and re-serialised the payload, key order and number formatting
    /// would come back different -- and the monitor's idea of a dashboard and
    /// the server's would have started to drift.
    #[tokio::test]
    async fn payloads_round_trip_byte_for_byte() {
        let h = Harness::new(&["alice"]).await;
        // Deliberately hostile: duplicate-looking key order, a float that would
        // renormalise, control characters, and non-ASCII.
        let payload = "{\"z\":1,\"a\":2.50,\"nested\":{\"t\":\"탭\\t줄\\n\"},\"e\":1e2}";

        h.put("/me/monitor/settings/dash:1", "alice", serde_json::json!({ "value": payload })).await;

        let (_, v) = h.get("/me/monitor/settings/dash:1", "alice").await;
        assert_eq!(v["value"].as_str().unwrap(), payload);

        let (_, v) = h.get("/me/monitor/settings", "alice").await;
        assert_eq!(v["entries"][0]["value"].as_str().unwrap(), payload);
    }

    // ── Versioning and the concurrent-write rule ───────────────────────────

    #[tokio::test]
    async fn versions_increment_and_report_what_was_replaced() {
        let h = Harness::new(&["alice"]).await;

        let (s, v) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "one" })).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["version"], 1);
        assert_eq!(v["created"], true);
        assert!(v["previous_version"].is_null());

        let (_, v) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "two" })).await;
        assert_eq!(v["version"], 2);
        assert_eq!(v["created"], false);
        assert_eq!(v["previous_version"], 1);

        // A re-created key starts over at 1, so "version 1" always means "this
        // entry is new" rather than "this entry is old".
        h.delete("/me/monitor/settings/k", "alice").await;
        let (_, v) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "three" })).await;
        assert_eq!(v["version"], 1);
    }

    /// Two devices, one entry. The loser has to be able to TELL it lost.
    #[tokio::test]
    async fn a_blind_writer_learns_it_clobbered_someone() {
        let h = Harness::new(&["alice"]).await;

        // Device A and device B both fetch version 1.
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "base" })).await;
        let fetched_version = 1;

        // Device A writes. It replaced exactly what it had read: it won cleanly.
        let (_, a) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "from-a" })).await;
        assert_eq!(a["previous_version"], fetched_version);

        // Device B writes blind, still holding version 1. Last write wins, so B
        // is now stored -- but previous_version says it overwrote version 2,
        // not the version 1 B had read. That mismatch is the signal.
        let (_, b) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "from-b" })).await;
        assert_eq!(b["previous_version"], 2);
        assert_ne!(b["previous_version"], fetched_version, "B must be able to see it clobbered A");

        let (_, v) = h.get("/me/monitor/settings/k", "alice").await;
        assert_eq!(v["value"], "from-b", "last write wins");
    }

    /// The same race, run by a client that would rather lose than clobber.
    #[tokio::test]
    async fn a_conditional_writer_is_refused_instead_of_clobbering() {
        let h = Harness::new(&["alice"]).await;
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "base" })).await;
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "from-a" })).await; // now v2

        let (s, v) = h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "from-b", "if_version": 1 }),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(v["current_version"], 2);
        assert!(v["current_updated_at"].is_i64());

        // Nothing was written.
        let (_, v) = h.get("/me/monitor/settings/k", "alice").await;
        assert_eq!(v["value"], "from-a");

        // Re-reading and retrying against the current version succeeds.
        let (s, v) = h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "from-b", "if_version": 2 }),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["version"], 3);
    }

    /// Deletion is a write too: a device holding an old index must not erase
    /// an edit that landed after that index was fetched.
    #[tokio::test]
    async fn a_conditional_delete_is_refused_instead_of_erasing_a_newer_edit() {
        let h = Harness::new(&["alice"]).await;
        h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "base" }),
        )
        .await;

        let (_, index) = h.get("/me/monitor/index", "alice").await;
        assert_eq!(index["delete_cas"], true);
        let fetched_version = index["entries"][0]["version"].as_i64().unwrap();

        h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "newer edit" }),
        )
        .await;

        let (status, conflict) = h
            .delete(
                &format!("/me/monitor/settings/k?if_version={fetched_version}"),
                "alice",
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["current_version"], 2);
        let (_, stored) = h.get("/me/monitor/settings/k", "alice").await;
        assert_eq!(stored["value"], "newer edit");

        let (status, _) = h
            .delete("/me/monitor/settings/k?if_version=2", "alice")
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    /// `if_version: 0` is create-only, so a client can publish a new dashboard
    /// without silently replacing one that already exists under that key.
    #[tokio::test]
    async fn if_version_zero_means_create_only() {
        let h = Harness::new(&["alice"]).await;

        let (s, v) = h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "first", "if_version": 0 }),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["version"], 1);

        let (s, v) = h.put(
            "/me/monitor/settings/k",
            "alice",
            serde_json::json!({ "value": "second", "if_version": 0 }),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(v["current_version"], 1);
    }

    // ── Size limits ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn an_oversized_value_is_refused_with_the_limit_named() {
        let h = Harness::new(&["alice"]).await;
        let big = "x".repeat(MAX_VALUE_BYTES + 1);

        let (s, v) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": big })).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(v["error"].as_str().unwrap().contains(&MAX_VALUE_BYTES.to_string()));

        // Nothing landed.
        let (s, _) = h.get("/me/monitor/settings/k", "alice").await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // Exactly at the cap is accepted.
        let ok = "x".repeat(MAX_VALUE_BYTES);
        let (s, _) = h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": ok })).await;
        assert_eq!(s, StatusCode::OK);
    }

    /// The body limit has to fire BEFORE the body is buffered -- that is the
    /// difference between refusing a 50 MB PUT and OOMing on it.
    #[tokio::test]
    async fn a_body_past_the_request_limit_is_refused() {
        let h = Harness::new(&["alice"]).await;
        let body = format!(r#"{{"value":"{}"}}"#, "x".repeat(MAX_REQUEST_BODY + 1024));

        let (s, _) = h
            .send(
                Request::builder()
                    .method("PUT")
                    .uri("/me/monitor/settings/k")
                    .header("authorization", format!("Bearer {}", h.token("alice")))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Quotas are checked against the post-write totals, in the transaction.
    /// Driven at the storage layer with a tiny quota because the HTTP ceiling
    /// (512 entries) cannot be reached inside the write rate limit.
    #[tokio::test]
    async fn per_user_quota_counts_entries_and_bytes() {
        let h = Harness::new(&["alice", "bob"]).await;
        let q = MonitorQuota { max_entries: 2, max_total_bytes: 100 };

        for key in ["a", "b"] {
            let out = h.db.upsert_monitor_setting("alice", key, "1234567890", None, q).await.unwrap();
            assert!(matches!(out, MonitorWriteOutcome::Written { .. }));
        }

        // Third distinct key is over the entry ceiling.
        match h.db.upsert_monitor_setting("alice", "c", "x", None, q).await.unwrap() {
            MonitorWriteOutcome::QuotaExceeded { what, used, limit } => {
                assert_eq!(what, "entries");
                assert_eq!((used, limit), (3, 2));
            }
            _ => panic!("expected an entry-count refusal"),
        }

        // Replacing an existing key with something bigger hits the byte ceiling.
        match h.db.upsert_monitor_setting("alice", "a", &"y".repeat(95), None, q).await.unwrap() {
            MonitorWriteOutcome::QuotaExceeded { what, used, limit } => {
                assert_eq!(what, "bytes");
                assert_eq!(limit, 100);
                assert_eq!(used, 105, "existing 10 for 'b' plus the proposed 95");
            }
            _ => panic!("expected a byte refusal"),
        }

        // Shrinking an entry is never refused for being over quota.
        let out = h.db.upsert_monitor_setting("alice", "a", "z", None, q).await.unwrap();
        assert!(matches!(out, MonitorWriteOutcome::Written { .. }));

        // The quota is alice's alone.
        let out = h.db.upsert_monitor_setting("bob", "a", "1234567890", None, q).await.unwrap();
        assert!(matches!(out, MonitorWriteOutcome::Written { .. }));
    }

    // ── Key validation ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn keys_outside_the_allowed_shape_are_refused() {
        let h = Harness::new(&["alice"]).await;
        for key in ["with space", "with%2Fslash", "tab\tkey", "홍길동", &"k".repeat(MAX_KEY_LEN + 1)] {
            let uri = format!("/me/monitor/settings/{}", urlencoding::encode(key));
            let (s, _) = h.put(&uri, "alice", serde_json::json!({ "value": "v" })).await;
            assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "key {key:?} should be refused");
        }

        for key in ["dash:main", "prefs.theme", "a_b-c", &"k".repeat(MAX_KEY_LEN)] {
            let (s, _) = h
                .put(&format!("/me/monitor/settings/{key}"), "alice", serde_json::json!({ "value": "v" }))
                .await;
            assert_eq!(s, StatusCode::OK, "key {key:?} should be accepted");
        }
    }

    // ── Bounded growth ─────────────────────────────────────────────────────

    /// A delete has to actually delete, and a deleted account must not leave
    /// its entries behind.
    #[tokio::test]
    async fn deletes_and_account_removal_leave_nothing_behind() {
        let h = Harness::new(&["alice"]).await;
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": "v" })).await;

        let (s, _) = h.delete("/me/monitor/settings/k", "alice").await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(h.db.get_monitor_setting("alice", "k").await.unwrap().is_none());
        let (s, _) = h.delete("/me/monitor/settings/k", "alice").await;
        assert_eq!(s, StatusCode::NOT_FOUND, "a second delete finds nothing");

        h.put("/me/monitor/settings/k2", "alice", serde_json::json!({ "value": "v" })).await;
        h.db.delete_user("alice").await.unwrap();
        assert!(
            h.db.list_monitor_settings("alice").await.unwrap().is_empty(),
            "deleting the account must cascade to its monitor settings"
        );
    }

    // ── Rate limiting ──────────────────────────────────────────────────────

    #[test]
    fn the_write_limiter_allows_a_burst_and_stops_a_loop() {
        let limiter = MonitorWriteRateLimiterInner::new();

        // A monitor saving its whole configuration writes many entries at once;
        // that must go through.
        for i in 0..MonitorWriteRateLimiterInner::MAX_WRITES {
            assert!(limiter.allow("alice").is_ok(), "write {i} of a burst was refused");
        }

        // The loop past the budget is not.
        let retry = limiter.allow("alice").expect_err("the budget must run out");
        assert!(retry >= 1 && retry <= MonitorWriteRateLimiterInner::WINDOW.as_secs());

        // And it is per user: bob is not throttled by alice.
        assert!(limiter.allow("bob").is_ok());
    }

    #[tokio::test]
    async fn writes_past_the_budget_answer_429_and_change_nothing() {
        let h = Harness::new(&["alice"]).await;

        for i in 0..MonitorWriteRateLimiterInner::MAX_WRITES {
            let (s, _) = h
                .put(&format!("/me/monitor/settings/k{i}"), "alice", serde_json::json!({ "value": "v" }))
                .await;
            assert_eq!(s, StatusCode::OK);
        }

        let (s, v) = h.put("/me/monitor/settings/over", "alice", serde_json::json!({ "value": "v" })).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert!(v["retry_after"].is_i64());
        assert!(h.db.get_monitor_setting("alice", "over").await.unwrap().is_none());

        // Deletes are a write path too.
        let (s, _) = h.delete("/me/monitor/settings/k0", "alice").await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert!(h.db.get_monitor_setting("alice", "k0").await.unwrap().is_some());

        // Reads are not throttled: a client that is told to back off still has
        // to be able to find out what the current state is.
        let (s, _) = h.get("/me/monitor/index", "alice").await;
        assert_eq!(s, StatusCode::OK);
    }

    // ── Index vs full listing ──────────────────────────────────────────────

    /// The index exists so a client can decide what to fetch without pulling
    /// the payloads. If it ever started carrying them, that is gone.
    #[tokio::test]
    async fn the_index_reports_size_without_carrying_the_payload() {
        let h = Harness::new(&["alice"]).await;
        let payload = "p".repeat(1234);
        h.put("/me/monitor/settings/k", "alice", serde_json::json!({ "value": payload })).await;

        let (_, v) = h.get("/me/monitor/index", "alice").await;
        let entry = &v["entries"][0];
        assert_eq!(entry["key"], "k");
        assert_eq!(entry["version"], 1);
        assert_eq!(entry["size_bytes"], 1234);
        assert!(entry["value"].is_null(), "the index must not carry payloads");
        assert!(entry["updated_at"].is_i64());

        assert_eq!(v["quota"]["used_bytes"], 1234);
        assert_eq!(v["quota"]["used_entries"], 1);
        assert_eq!(v["quota"]["max_value_bytes"], MAX_VALUE_BYTES);
    }

    #[tokio::test]
    async fn an_empty_account_lists_nothing_rather_than_erroring() {
        let h = Harness::new(&["alice"]).await;

        let (s, v) = h.get("/me/monitor/settings", "alice").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);

        let (s, v) = h.get("/me/monitor/index", "alice").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);
        assert_eq!(v["quota"]["used_bytes"], 0);
    }
}
