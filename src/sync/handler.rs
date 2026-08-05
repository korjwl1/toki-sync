use std::io::Read;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::events::{EventStore, ServerEvent};
use super::protocol::*;

const MAX_DECOMPRESSED_SIZE: usize = 64 * 1024 * 1024; // 64 MiB
const MAX_BATCH_SIZE: usize = 50_000;

/// Handle a single TCP client connection.
/// Per-user floor between accepted `SyncWindows` batches, shared by every
/// connection so reconnecting cannot reset it. The client throttles itself to
/// 5 minutes; this only catches buggy or hostile peers.
pub type WindowsRateLimiter = Arc<WindowsRateLimiterInner>;

#[derive(Default)]
pub struct WindowsRateLimiterInner {
    last: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl WindowsRateLimiterInner {
    const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    /// Sweep threshold: the map holds one small entry per active user, and
    /// stale entries are dropped opportunistically so it cannot grow with the
    /// total number of users ever seen.
    const SWEEP_AT: usize = 4096;

    pub fn allow(&self, user_id: &str) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = map.get(user_id) {
            if now.duration_since(*prev) < Self::MIN_INTERVAL {
                return false;
            }
        }
        if map.len() >= Self::SWEEP_AT {
            map.retain(|_, t| now.duration_since(*t) < Self::MIN_INTERVAL);
        }
        map.insert(user_id.to_string(), now);
        true
    }
}


/// Server-side per-batch keep limit. The client applies the same cap before
/// sending (`MAX_WINDOWS_PER_SYNC` in toki), so reaching it means a misbehaving
/// or outdated client.
pub(crate) const MAX_WINDOW_ITEMS: usize = 2_000;

pub(crate) struct SanitizedWindows {
    pub items: Vec<toki_sync_protocol::WireWindow>,
    /// Rows that can NEVER become valid by waiting. Excluded silently: telling
    /// the client would mean a permanent 5-minute resend loop.
    pub stable_rejects: usize,
    /// Rows outside the horizon that time can fix. Reported, so the client
    /// retries them.
    pub transient_rejects: usize,
    /// Rows beyond the per-batch cap. Like stable rejects, not reported.
    pub over_cap: usize,
}

/// Split by whether waiting can make the item valid, because the two need
/// OPPOSITE answers. A transient reject must feed `dropped` -> SyncErr, so the
/// client keeps retrying until its clock catches up. A stable reject must NOT:
/// the client resends the identical cursorless set every cycle, so SyncErr for
/// a permanently-invalid row means the fingerprint never latches and the device
/// re-scans, re-serializes and re-uploads its whole 60-day set every 5 minutes,
/// forever, with `sync_last_error` stuck. (A >64-byte `plan` from a new plan
/// tier is enough to trigger that.) Same reasoning as `over_cap`.
fn window_item_stable_ok(w: &toki_sync_protocol::WireWindow, now_ms: i64) -> bool {
    fn str_ok(s: &str, max: usize) -> bool {
        s.len() <= max && !s.chars().any(|c| c.is_control())
    }
    str_ok(&w.limit_id, 64)
        && str_ok(&w.account, 64)
        && str_ok(&w.plan, 64)
        && w.window_kind <= 1
        && w.window_minutes > 0
        && w.window_minutes <= 60 * 24 * 31
        && w.first_seen_ms >= 0
        && w.limit_reached_kind <= 2
        && w.sampled_active_fraction <= 1000
        && w.last_sample_gap_ms >= -86_400_000
        && (w.first_seen_ms == 0 || w.first_seen_ms <= w.observed_ts_ms)
        // Too old only gets older: unrecoverable, so stable.
        && w.window_end_ms > now_ms - 2 * 365 * 86_400_000
        && w.raw_resets_at_ms > now_ms - 2 * 365 * 86_400_000
}

/// Only upper bounds: a row dated past the horizon becomes valid as the server
/// clock advances (or the client's corrects), so it is worth retrying.
fn window_item_transient_ok(w: &toki_sync_protocol::WireWindow, now_ms: i64) -> bool {
    w.window_end_ms < now_ms + 40 * 86_400_000
        // Bounded because the query path does arithmetic on it (raw + grace);
        // an unbounded value can wrap in release.
        && w.raw_resets_at_ms < now_ms + 41 * 86_400_000
}

/// Validate, clamp and cap one uploaded batch. Never rejects the whole batch:
/// the client resends the identical cursorless set every cycle, so one
/// malformed row would otherwise dead-letter that device's window sync for up
/// to 60 days.
pub(crate) fn sanitize_window_batch(
    mut items: Vec<toki_sync_protocol::WireWindow>,
    now_ms: i64,
) -> SanitizedWindows {
    // Clamp `observed_ts_ms` UP into range rather than rejecting, symmetric
    // with the upper clamp below. A machine that booted before NTP converged
    // stamps observations near epoch, and `merge_from` only advances
    // observed_ts on `>=`, so the bad stamp would otherwise persist for the
    // full 60-day horizon.
    let observed_floor = now_ms - 2 * 365 * 86_400_000;
    for w in &mut items {
        if w.observed_ts_ms < observed_floor {
            w.observed_ts_ms = observed_floor;
        }
    }
    let mut stable_rejects = 0usize;
    items.retain(|w| {
        let ok = window_item_stable_ok(w, now_ms);
        if !ok {
            stable_rejects += 1;
        }
        ok
    });
    let after_stable = items.len();
    items.retain(|w| window_item_transient_ok(w, now_ms));
    let transient_rejects = after_stable - items.len();

    // Enforce the local tracker's invariant server-side too: a compromised or
    // buggy device could otherwise store an active_ms large enough to overflow
    // the monitor's Int64 summation and wedge Plan Fit for the whole account.
    for w in &mut items {
        let cap = (w.window_minutes as u64).saturating_mul(60_000);
        if w.active_ms > cap {
            w.active_ms = cap;
        }
    }

    // Validation runs BEFORE this truncation, so a skewed clock's far-future
    // rows cannot sort to the front and evict real data. Keep that order.
    let mut over_cap = 0usize;
    if items.len() > MAX_WINDOW_ITEMS {
        // Keep the NEWEST rows rather than rejecting the batch: the client
        // resends the same cursorless set every cycle, so a whole-batch reject
        // stored zero rows forever (a machine with many distinct accounts can
        // legitimately exceed the cap).
        over_cap = items.len() - MAX_WINDOW_ITEMS;
        items.sort_by_key(|w| std::cmp::Reverse(w.window_end_ms));
        items.truncate(MAX_WINDOW_ITEMS);
    }

    // Clock-skew defense (plan F10): a future-dated client clock must not
    // permanently win last-writer fields.
    let clamp = now_ms + 60_000;
    for w in &mut items {
        if w.observed_ts_ms > clamp {
            w.observed_ts_ms = clamp;
        }
        // Keep the validated invariant true AFTER clamping: it was checked
        // against the pre-clamp observed_ts.
        if w.first_seen_ms > w.observed_ts_ms {
            w.first_seen_ms = w.observed_ts_ms;
        }
    }

    SanitizedWindows { items, stable_rejects, transient_rejects, over_cap }
}

/// Pre-merge in-batch duplicates (the same logical window twice in one
/// payload): stores upsert item-by-item, and without this a later low-peak
/// duplicate could overwrite an earlier high peak inside the same batch.
///
/// Sort + merge adjacent, not a HashMap: the map cost two String clones and a
/// bucket per item (~4000 allocations per upload) to defend against duplicates
/// the client structurally cannot produce — its local key is this exact tuple,
/// so one key yields one row.
pub(crate) fn premerge_windows(
    mut items: Vec<toki_sync_protocol::WireWindow>,
) -> Vec<toki_sync_protocol::WireWindow> {
    items.sort_unstable_by(|a, b| {
        a.window_kind
            .cmp(&b.window_kind)
            .then_with(|| a.limit_id.cmp(&b.limit_id))
            .then_with(|| a.account.cmp(&b.account))
            .then_with(|| a.window_end_ms.cmp(&b.window_end_ms))
    });
    let mut merged: Vec<toki_sync_protocol::WireWindow> = Vec::with_capacity(items.len());
    for w in items.drain(..) {
        match merged.last_mut() {
            Some(prev)
                if prev.window_kind == w.window_kind
                    && prev.limit_id == w.limit_id
                    && prev.account == w.account
                    && prev.window_end_ms == w.window_end_ms =>
            {
                crate::events::merge_wire_windows(prev, &w)
            }
            _ => merged.push(w),
        }
    }
    merged
}

pub async fn handle_connection(
    stream: TcpStream,
    db: Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    events: Arc<dyn EventStore>,
    batch_semaphore: Arc<Semaphore>,
    dedup_retention_secs: i64,
    active_cache: crate::server::http::ActiveCache,
    windows_rate: WindowsRateLimiter,
) -> Result<()> {
    let (r, w) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(r);
    let mut writer = tokio::io::BufWriter::new(w);

    // ── AUTH ────────────────────────────────────────────────────────────────
    // Pre-auth hardening: an unauthenticated peer gets a small payload cap and
    // a short deadline. Without both, 500 connections could each announce a
    // MAX_PAYLOAD_SIZE frame and stall — GiBs of allocation and every
    // connection slot held, all before a single JWT is checked.
    const AUTH_MAX_PAYLOAD: u32 = 64 * 1024;
    const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let (msg_type, payload) = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_frame_limited(&mut reader, AUTH_MAX_PAYLOAD),
    )
    .await
    {
        Ok(frame) => frame?,
        Err(_) => {
            tracing::warn!("auth handshake timed out");
            return Ok(());
        }
    };
    if msg_type != MsgType::Auth {
        return Err(anyhow::anyhow!("expected AUTH, got {msg_type:?}"));
    }

    let auth: AuthPayload = bincode::deserialize(&payload)?;

    // Protocol version check — reject unknown versions immediately
    if auth.protocol_version != PROTOCOL_VERSION {
        let err = AuthErrPayload {
            reason: format!(
                "unsupported protocol version: client={}, server={}",
                auth.protocol_version, PROTOCOL_VERSION
            ),
            reset_required: false,
        };
        write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
        return Ok(());
    }

    // JWT verification first — we need user_id to scope any device operations
    let claims = match jwt.verify_access(&auth.jwt) {
        Ok(c) => c,
        Err(e) => {
            let err = AuthErrPayload {
                reason: format!("JWT invalid: {e}"),
                reset_required: false,
            };
            write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
            return Ok(());
        }
    };

    let user_id  = claims.sub.clone();
    let provider = auth.provider.clone();

    // Reject deactivated (or deleted) accounts even while their access token is
    // still within its TTL — same shared cache/semantics as the HTTP path.
    match crate::server::http::is_user_active_cached(&active_cache, &*db, &user_id).await {
        Ok(true) => {}
        Ok(false) => {
            let err = AuthErrPayload { reason: "account deactivated".to_string(), reset_required: false };
            write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
            return Ok(());
        }
        Err(e) => {
            let err = AuthErrPayload { reason: format!("active check failed: {e}"), reset_required: false };
            write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
            return Ok(());
        }
    }

    // Find or create device using the stable device_key UUID
    let device_name = crate::server::http::truncate_device_name(&auth.device_name);
    // Validate device_key is a well-formed UUID (prevents Fjall key injection via null bytes)
    if uuid::Uuid::parse_str(&auth.device_key).is_err() {
        let err = AuthErrPayload {
            reason: format!("invalid device_key: expected UUID format"),
            reset_required: false,
        };
        write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
        return Ok(());
    }

    let device_id = find_or_create_device(&*db, &user_id, device_name, &auth.device_key).await?;

    // Schema version guard — delete this device's events and reset cursor
    if auth.schema_version != SCHEMA_VERSION {
        if let Err(e) = events.delete_device_events(&device_id).await {
            tracing::warn!("failed to delete events for device {device_id}: {e}");
        }
        // Reset server cursor so client re-syncs all data
        if let Err(e) = db.reset_cursor(&device_id, &provider).await {
            tracing::warn!("failed to reset cursor for device {device_id}: {e}");
        }

        let err = AuthErrPayload {
            reason: format!(
                "schema version mismatch: client={}, server={}",
                auth.schema_version, SCHEMA_VERSION
            ),
            reset_required: true,
        };
        write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
        return Ok(());
    }

    // Ensure cursor row for this (device, provider)
    db.ensure_cursor(&device_id, &provider).await?;

    // AUTH_OK
    let ok = AuthOkPayload { device_id: device_id.clone() };
    write_frame(&mut writer, MsgType::AuthOk, &bincode::serialize(&ok)?).await?;

    tracing::debug!("sync auth ok: user={user_id} device={device_id} provider={provider}");

    // ── Main loop ────────────────────────────────────────────────────────────
    // Read timeout: 2 missed PING cycles (client sends every 60s) → disconnect.
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    loop {
        let (msg_type, payload) = match tokio::time::timeout(READ_TIMEOUT, read_frame(&mut reader)).await {
            Err(_elapsed) => {
                tracing::warn!("TCP read timeout ({READ_TIMEOUT:?}), dropping connection");
                break;
            }
            Ok(Ok(f)) => f,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!("dropping TCP connection: {e}");
                break;
            }
            Ok(Err(e)) => return Err(e.into()),
        };

        match msg_type {
            MsgType::GetLastTs => {
                let get_ts: GetLastTsPayload = bincode::deserialize(&payload)?;
                let ts = db.get_last_ts(&device_id, &get_ts.provider).await?;
                let p = LastTsPayload { ts_ms: ts };
                write_frame(&mut writer, MsgType::LastTs, &bincode::serialize(&p)?).await?;
            }

            MsgType::SyncBatch | MsgType::SyncBatchZstd => {
                // Re-check active status per batch so a mid-session deactivation
                // stops further writes (cache TTL <= 60s, or instantly on evict),
                // not just new connections.
                match crate::server::http::is_user_active_cached(&active_cache, &*db, &user_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let err = SyncErrPayload { reason: "account deactivated".to_string() };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("active check failed for device={device_id}: {e}");
                        break;
                    }
                }
                let raw = if msg_type == MsgType::SyncBatchZstd {
                    let decoder = zstd::stream::Decoder::new(payload.as_slice())
                        .map_err(|e| anyhow::anyhow!("zstd decoder init failed: {e}"))?;
                    let mut buf = Vec::new();
                    decoder.take(MAX_DECOMPRESSED_SIZE as u64 + 1).read_to_end(&mut buf)
                        .map_err(|e| anyhow::anyhow!("zstd decompress failed: {e}"))?;
                    if buf.len() > MAX_DECOMPRESSED_SIZE {
                        anyhow::bail!("decompressed payload exceeds {MAX_DECOMPRESSED_SIZE} bytes");
                    }
                    buf
                } else {
                    payload
                };
                let batch: SyncBatchPayload = bincode::deserialize(&raw)?;
                // Ensure cursor exists for this batch's provider (may differ from auth provider)
                db.ensure_cursor(&device_id, &batch.provider).await?;
                match handle_sync_batch(&batch, &user_id, &device_id, &batch.provider, &*db, &*events, &batch_semaphore, dedup_retention_secs).await {
                    Ok(last_ts) => {
                        let ack = SyncAckPayload { last_ts_ms: last_ts };
                        write_frame(&mut writer, MsgType::SyncAck, &bincode::serialize(&ack)?).await?;
                    }
                    Err(e) => {
                        tracing::warn!("sync_batch error for device={device_id}: {e}");
                        let err = SyncErrPayload { reason: e.to_string() };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    }
                }
            }

            MsgType::SyncWindows => {
                // Keyed by USER and shared across connections: a per-connection
                // throttle is reset by reconnecting, so a peer holding one valid
                // JWT could loop connect -> upload -> disconnect and grow stored
                // rows without bound (every distinct limit_id/account tuple is a
                // new key, and retention only removes OLD anchors).
                if !windows_rate.allow(&user_id) {
                    tracing::debug!("sync_windows throttled for user={user_id} device={device_id}");
                    let err = SyncErrPayload { reason: "windows sync throttled".to_string() };
                    write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    continue;
                }
                match crate::server::http::is_user_active_cached(&active_cache, &*db, &user_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let err = SyncErrPayload { reason: "account deactivated".to_string() };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("active check failed for device={device_id}: {e}");
                        break;
                    }
                }
                // Bound BEFORE deserializing: the post-auth frame limit is
                // MAX_PAYLOAD_SIZE (16 MiB), which would expand to ~171k
                // WireWindows (~40 MB live) per connection — far past the
                // 2000-item cap enforced below. A legitimate full 60-day set
                // is well under 1 MiB.
                const MAX_WINDOWS_PAYLOAD: usize = 1024 * 1024;
                if payload.len() > MAX_WINDOWS_PAYLOAD {
                    let err = SyncErrPayload {
                        reason: format!(
                            "windows payload too large: {} bytes (max {MAX_WINDOWS_PAYLOAD})",
                            payload.len()
                        ),
                    };
                    write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    continue;
                }
                let mut win: toki_sync_protocol::SyncWindowsPayload = bincode::deserialize(&payload)?;
                if win.windows_schema != toki_sync_protocol::WINDOWS_SCHEMA_VERSION {
                    let err = SyncErrPayload {
                        reason: format!(
                            "unsupported windows_schema {} (server supports {})",
                            win.windows_schema,
                            toki_sync_protocol::WINDOWS_SCHEMA_VERSION
                        ),
                    };
                    write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    continue;
                }
                // Data hygiene: provider names come from the client; reject
                // anything outside the known shape before it becomes row keys.
                let provider_ok = !win.provider.is_empty()
                    && win.provider.len() <= 32
                    && win.provider.chars().all(|c| c.is_ascii_lowercase() || c == '_');
                if !provider_ok {
                    let err = SyncErrPayload {
                        reason: format!("invalid provider name: {:?}", win.provider),
                    };
                    write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    continue;
                }
                // Windows are ~2/day/limit — a 60-day resend tops out around
                // a thousand rows; anything past this is a misbehaving client
                // and gets bounced before the String-heavy premerge.
                // Per-item hygiene: these strings become storage keys (NUL is
                // the fjall key delimiter) and the timestamps drive retention.
                let now_ms_v = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let before = win.items.len();
                let clean = sanitize_window_batch(std::mem::take(&mut win.items), now_ms_v);
                if clean.stable_rejects > 0 {
                    tracing::warn!(
                        "sync_windows: dropped {}/{before} permanently-invalid items \
                         for device={device_id} (not retried)",
                        clean.stable_rejects
                    );
                }
                if clean.transient_rejects > 0 {
                    tracing::warn!(
                        "sync_windows: deferred {}/{before} out-of-horizon items for device={device_id}",
                        clean.transient_rejects
                    );
                }
                if clean.over_cap > 0 {
                    tracing::warn!(
                        "sync_windows: kept newest {MAX_WINDOW_ITEMS}, dropped {} over-cap items for device={device_id}",
                        clean.over_cap
                    );
                }
                let dropped_transient = clean.transient_rejects;
                let over_cap = clean.over_cap;
                let mut items = clean.items;
                // Write-pressure valve around the premerge+upsert ONLY. The
                // permit must not span the response write: a peer that stops
                // reading blocks write_frame indefinitely (no write timeout
                // exists), and ten such connections would drain the semaphore
                // and stall every user's ingestion. handle_sync_batch drops
                // its permit before acking for exactly this reason.
                let (upsert_result, skipped) = {
                    let _write_permit = batch_semaphore
                        .acquire()
                        .await
                        .map_err(|e| anyhow::anyhow!("write semaphore closed: {e}"))?;
                    // Pre-merge in-batch duplicates (same logical window twice
                    // in one payload): stores upsert item-by-item, and without
                    // this a later low-peak duplicate could overwrite an
                    // earlier high peak inside the same ClickHouse batch.
                    items = premerge_windows(items);
                    let r = events.upsert_windows(&user_id, &win.provider, &items).await;
                    let skipped = r.as_ref().copied().unwrap_or(0);
                    (r.map(|_| ()), skipped)
                };
                // Rows the store could not write (e.g. a future value version
                // it must preserve) count as not-accepted, so the client is
                // not told to latch its fingerprint over them. `over_cap` is
                // deliberately EXCLUDED: it is a stable property of the
                // device's set, so answering SyncErr for it would mean a
                // permanent 5-minute resend loop that can never succeed. The
                // client applies the same cap before sending (see
                // MAX_WINDOWS_PER_SYNC), so reaching this is a misbehaving or
                // outdated client, not the normal path.
                let dropped = dropped_transient + skipped;
                let _ = over_cap;
                match upsert_result {
                    Ok(()) if dropped == 0 => {
                        let last = items.iter().map(|w| w.observed_ts_ms).max().unwrap_or(0);
                        let ack = SyncAckPayload { last_ts_ms: last };
                        write_frame(&mut writer, MsgType::SyncAck, &bincode::serialize(&ack)?).await?;
                    }
                    Ok(()) => {
                        // Valid items are stored, but the batch was NOT fully
                        // accepted: answering SyncAck would let the client
                        // confirm its fingerprint and skip every future resend
                        // of this set, so a row that becomes valid later (a
                        // corrected clock) would never be retried.
                        let err = SyncErrPayload {
                            reason: format!("{dropped} of {before} window items rejected"),
                        };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    }
                    Err(e) => {
                        tracing::warn!("sync_windows error for device={device_id}: {e}");
                        let err = SyncErrPayload { reason: e.to_string() };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    }
                }
            }

            MsgType::Ping => {
                write_empty_frame(&mut writer, MsgType::Pong).await?;
            }

            other => {
                tracing::warn!("unexpected msg_type in main loop: {other:?}");
            }
        }
    }

    Ok(())
}

// ─── DB helpers ──────────────────────────────────────────────────────────────

async fn find_or_create_device(
    db: &dyn DatabaseRepo,
    user_id: &str,
    device_name: &str,
    device_key: &str,
) -> Result<String> {
    // Use client's device_key as the device ID directly.
    // This ensures the same physical device always has the same ID,
    // even after disable/re-enable or server DB rebuild.
    if let Some(id) = db.find_device_by_key_and_user(device_key, user_id).await? {
        db.update_device_seen(&id, device_name).await?;
        return Ok(id);
    }

    // New device: use device_key as ID (not a random UUID)
    db.create_device(device_key, user_id, device_name, device_key).await?;

    tracing::info!("registered device '{device_name}' (id={device_key}) for user={user_id}");
    Ok(device_key.to_string())
}

async fn handle_sync_batch(
    batch: &SyncBatchPayload,
    user_id: &str,
    device_id: &str,
    provider: &str,
    db: &dyn DatabaseRepo,
    events: &dyn EventStore,
    batch_semaphore: &Semaphore,
    dedup_retention_secs: i64,
) -> Result<i64> {
    if batch.items.is_empty() {
        let current = db.get_last_ts(device_id, provider).await?;
        return Ok(current);
    }

    if batch.items.len() > MAX_BATCH_SIZE {
        anyhow::bail!("batch too large: {} items (max {MAX_BATCH_SIZE})", batch.items.len());
    }

    // Convert SyncItems to ServerEvents (resolve dict IDs to strings)
    let server_events: Vec<ServerEvent> = batch.items.iter().map(|item| {
        let model = batch.dict.get(&item.event.model_id).cloned().unwrap_or_else(|| {
            tracing::warn!("missing dict ID {} for model in device {}", item.event.model_id, device_id);
            String::new()
        });
        let project = batch.dict.get(&item.event.project_name_id)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| {
                if item.event.project_name_id != 0 {
                    tracing::warn!("missing dict ID {} for project in device {}", item.event.project_name_id, device_id);
                }
                String::new()
            });
        let bare_msg_id = item.message_id.split(':').next().unwrap_or(&item.message_id);

        // Map token columns by name (supports different providers)
        let mut se = ServerEvent {
            device_id: device_id.to_string(),
            user_id: user_id.to_string(),
            msg_id: bare_msg_id.to_string(),
            ts_ms: item.ts_ms,
            provider: provider.to_string(),
            model,
            project,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            usage_total: 0,
        };

        for (i, col) in batch.token_columns.iter().enumerate() {
            if i >= item.event.tokens.len() { break; }
            match col.as_str() {
                "input" => se.input_tokens = item.event.tokens[i],
                "output" => se.output_tokens = item.event.tokens[i],
                "cache_create" => se.cache_creation_input_tokens = item.event.tokens[i],
                "cache_read" => se.cache_read_input_tokens = item.event.tokens[i],
                // Codex subsets: store in the corresponding fields but they're
                // already excluded from usage_total by the daemon
                "cached_input" => se.cache_read_input_tokens = item.event.tokens[i],
                "reasoning_output" => se.cache_creation_input_tokens = item.event.tokens[i],
                _ => {}
            }
        }

        // usage_total is pre-computed by the daemon per provider:
        // - Claude: input + output + cache_creation + cache_read
        // - Codex: input + output only (cached_input ⊂ input, reasoning_output ⊂ output)
        // We trust the daemon's calculation rather than recomputing, because
        // the server doesn't know provider-specific semantics at this point.
        se.usage_total = item.usage_total;

        se
    }).collect();

    // Acquire permit (limits concurrent writes to EventStore)
    let permit = batch_semaphore.acquire().await
        .map_err(|_| anyhow::anyhow!("batch semaphore closed"))?;

    // Write to EventStore — dedup by (device_id, provider, msg_id) is handled internally
    events.upsert_events(&server_events).await?;

    drop(permit);

    // Advance cursor to max ts in this batch
    let max_ts = batch.items.iter().map(|i| i.ts_ms).max().unwrap_or(0);
    db.advance_cursor(device_id, provider, max_ts).await?;

    // Clean up dedup index entries older than the retention window.
    // Retention is generous (default 30 days) so a late correction/update for
    // an old message still hits the idx and dedups instead of inserting a
    // duplicate row. idx_msg entries are tiny compared to events. Only run when
    // the cutoff is positive (device has more than one retention window of data)
    // to avoid scanning on every batch for new devices.
    let cutoff_ms = max_ts - dedup_retention_secs * 1000;
    if cutoff_ms > 0 {
        if let Err(e) = events.cleanup_old_dedup(device_id, cutoff_ms).await {
            tracing::warn!("idx_msg cleanup failed for device {device_id}: {e}");
        }
    }

    Ok(max_ts)
}


#[cfg(test)]
mod window_batch_tests {
    use super::*;
    use toki_sync_protocol::WireWindow;

    const NOW: i64 = 1_800_000_000_000;

    fn base() -> WireWindow {
        WireWindow {
            window_kind: 0,
            limit_id: "five_hour".into(),
            account: "acct".into(),
            window_end_ms: NOW - 3_600_000,
            raw_resets_at_ms: NOW - 3_600_000,
            window_minutes: 300,
            peak_pct_x100: 5000,
            last_pct_x100: 5000,
            observed_ts_ms: NOW - 3_601_000,
            first_seen_ms: NOW - 3_900_000,
            finalized: true,
            maxed_out: false,
            limit_reached_kind: 0,
            time_to_100_ms: -1,
            active_ms: 60_000,
            last_sample_gap_ms: 1000,
            sampled_active_fraction: 1000,
            n_samples: 5,
            plan: "max_5x".into(),
        }
    }

    /// The whole point of the stable/transient split: a permanently-invalid row
    /// must NOT be reported, because the client resends the same set forever
    /// and a SyncErr would make it re-upload its 60-day set every 5 minutes
    /// with no way to ever succeed.
    #[test]
    fn permanently_invalid_rows_are_dropped_without_being_reported() {
        let mut long_plan = base();
        long_plan.plan = "p".repeat(65);
        let mut bad_kind = base();
        bad_kind.window_kind = 7;
        let mut ctrl = base();
        ctrl.limit_id = "five\u{0}hour".into();

        let out = sanitize_window_batch(vec![base(), long_plan, bad_kind, ctrl], NOW);
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.stable_rejects, 3);
        assert_eq!(out.transient_rejects, 0, "these can never become valid; must not be retried");
    }

    /// The mirror case: a row the server clock will eventually accept MUST be
    /// reported so the client tries again.
    #[test]
    fn out_of_horizon_rows_are_reported_for_retry() {
        let mut future = base();
        future.window_end_ms = NOW + 41 * 86_400_000;
        future.raw_resets_at_ms = NOW + 41 * 86_400_000;

        let out = sanitize_window_batch(vec![base(), future], NOW);
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.transient_rejects, 1);
        assert_eq!(out.stable_rejects, 0);
    }

    /// An unconverged-NTP boot stamps observations near epoch. Rejecting would
    /// strand the row for the full horizon; clamping keeps it.
    #[test]
    fn epoch_observed_ts_is_clamped_up_not_rejected() {
        let mut old = base();
        old.observed_ts_ms = 1000;
        old.first_seen_ms = 0;

        let out = sanitize_window_batch(vec![old], NOW);
        assert_eq!(out.stable_rejects, 0);
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].observed_ts_ms, NOW - 2 * 365 * 86_400_000);
    }

    /// A future-dated client clock must not permanently win last-writer fields,
    /// and the first_seen <= observed invariant must survive the clamp.
    #[test]
    fn future_clock_is_clamped_and_first_seen_follows() {
        let mut skewed = base();
        skewed.observed_ts_ms = NOW + 10 * 86_400_000;
        skewed.first_seen_ms = NOW + 9 * 86_400_000;

        let out = sanitize_window_batch(vec![skewed], NOW);
        let w = &out.items[0];
        assert_eq!(w.observed_ts_ms, NOW + 60_000);
        assert!(w.first_seen_ms <= w.observed_ts_ms);
    }

    /// active_ms above the window's own length would overflow the monitor's
    /// summation and wedge Plan Fit for the whole account.
    #[test]
    fn active_ms_is_capped_at_the_window_length() {
        let mut huge = base();
        huge.active_ms = u64::MAX;
        let out = sanitize_window_batch(vec![huge], NOW);
        assert_eq!(out.items[0].active_ms, 300 * 60_000);
    }

    /// Validation must run BEFORE the cap: otherwise far-future rows from a
    /// skewed clock sort to the front and evict real data.
    #[test]
    fn over_cap_keeps_newest_valid_rows_and_is_not_reported() {
        let mut items: Vec<WireWindow> = Vec::new();
        for i in 0..(MAX_WINDOW_ITEMS as i64 + 10) {
            let mut w = base();
            w.limit_id = format!("l{i}");
            w.window_end_ms = NOW - i * 600_000;
            w.raw_resets_at_ms = w.window_end_ms;
            items.push(w);
        }
        // A far-future row that would sort first if it survived validation.
        let mut evil = base();
        evil.limit_id = "evil".into();
        evil.window_end_ms = NOW + 39 * 86_400_000;
        evil.raw_resets_at_ms = NOW + 39 * 86_400_000;
        items.push(evil);

        let out = sanitize_window_batch(items, NOW);
        assert_eq!(out.items.len(), MAX_WINDOW_ITEMS);
        assert_eq!(out.over_cap, 11);
        assert_eq!(out.transient_rejects, 0, "over-cap is stable; must not trigger a retry loop");
        let oldest = out.items.iter().map(|w| w.window_end_ms).min().unwrap();
        assert!(oldest > NOW - (MAX_WINDOW_ITEMS as i64 + 9) * 600_000);
    }

    /// Stores upsert item-by-item, so a later low-peak duplicate inside one
    /// payload could otherwise overwrite an earlier high peak.
    #[test]
    fn premerge_takes_the_max_peak_regardless_of_order() {
        let mut high = base();
        high.peak_pct_x100 = 9500;
        high.observed_ts_ms = NOW - 10_000;
        let mut low = base();
        low.peak_pct_x100 = 1000;
        low.observed_ts_ms = NOW - 5_000; // later observation, lower peak
        low.finalized = true;

        let merged = premerge_windows(vec![high, low]);
        assert_eq!(merged.len(), 1, "same identity must collapse to one row");
        assert_eq!(merged[0].peak_pct_x100, 9500);
        assert_eq!(merged[0].observed_ts_ms, NOW - 5_000);
    }

    /// Different identities must never collapse — a merge across limit ids or
    /// accounts would blend one limit's peak into another's row.
    #[test]
    fn premerge_keeps_distinct_identities_apart() {
        let mut a = base();
        a.limit_id = "five_hour".into();
        let mut b = base();
        b.limit_id = "seven_day".into();
        let mut c = base();
        c.account = "other".into();
        let mut d = base();
        d.window_end_ms = base().window_end_ms + 1;

        assert_eq!(premerge_windows(vec![a, b, c, d]).len(), 4);
    }

    /// The per-user limiter exists because the previous per-connection one was
    /// reset by reconnecting.
    #[test]
    fn windows_rate_limiter_is_per_user_and_survives_reconnect() {
        let limiter = WindowsRateLimiterInner::default();
        assert!(limiter.allow("user-a"), "first batch accepted");
        assert!(!limiter.allow("user-a"), "second batch inside the window refused");
        assert!(limiter.allow("user-b"), "a different user is unaffected");
        // A "reconnect" is just another call with the same user id.
        assert!(!limiter.allow("user-a"));
    }
}
