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
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::db::models::{MonitorQuota, MonitorWriteOutcome};

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

pub async fn me_monitor_delete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
    let claims = authenticate(&state, &headers).await?;
    validate_key(&key)?;

    if let Err(retry_after) = state.monitor_rate.allow(&claims.sub) {
        return Ok(too_many_writes(retry_after));
    }

    let deleted = state
        .db
        .delete_monitor_setting(&claims.sub, &key)
        .await
        .map_err(AppError::internal)?;

    if !deleted {
        return Err(AppError::not_found("monitor setting not found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
