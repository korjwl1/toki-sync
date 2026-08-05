use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use fjall::{Database as FjallDatabase, Keyspace, KeyspaceCreateOptions};

use super::{EventStore, ServerEvent, UserFilter};

/// Event store schema version. Auto-resets Fjall data on mismatch.
///
/// History:
/// - v1: initial (device_id\0msg_id dedup key)
/// - v2: added provider to dedup key (device_id\0provider\0msg_id)
const EVENT_SCHEMA_VERSION: u32 = 2;

/// Fjall-backed event store with msg_id dedup.
///
/// Replicates the local daemon's dedup pattern (toki/src/db.rs):
/// - `events` keyspace: sorted by [ts_ms(8 BE)][device_id\0msg_id]
/// - `idx_msg` keyspace: [device_id\0provider\0msg_id] → events_key (dedup lookup)
/// - `idx_user` keyspace: [user_id\0][ts_ms(8 BE)][device_id\0msg_id] → event value.
///   Secondary index so `scope=self`/`scope=team` queries scan only the relevant
///   users' events instead of every user's. Holds an inline copy of the event so
///   user-scoped queries need no second lookup.
///
/// On upsert: if (device_id, provider, msg_id) already exists, delete old event, insert new.
/// Atomic via OwnedWriteBatch.
///
/// All fields (`FjallDatabase`, `Keyspace`) are internally `Arc`-wrapped and Clone,
/// so they can be safely moved into `spawn_blocking` closures without unsafe code.
pub struct FjallEventStore {
    db: FjallDatabase,
    events: Keyspace,
    idx_msg: Keyspace,
    idx_user: Keyspace,
    /// Rate-limit windows: [user_id\0provider\0limit_id\0account\0][kind u8][window_end_ms i64 BE]
    /// → [version u8][bincode WireWindow]. The version byte decouples the
    /// persisted layout from protocol evolution: unknown (future) versions
    /// are PRESERVED, never clobbered, by older binaries.
    ///
    /// Additive keyspace: adding it does not bump EVENT_SCHEMA_VERSION. It is
    /// NOT protected from a version reset, though — that path removes the
    /// whole directory. Clients resend their full 60-day set within 5 minutes,
    /// so only the 60d-730d retention tail is lost, and only on an events
    /// schema bump.
    windows: Keyspace,
    /// Serializes window read-merge-write cycles. Deliberately separate from
    /// mutation_lock: window traffic must not queue behind event-batch index
    /// mutations (and vice versa).
    windows_lock: Arc<Mutex<()>>,
    /// Serializes index-mutating operations (upsert / cleanup / delete).
    ///
    /// Fjall commits a batch atomically, but each op first *reads* the committed
    /// idx_msg predecessor and then commits a dependent write. Without this lock
    /// two concurrent upserts could both observe the same predecessor and both
    /// commit (double count), and a cleanup could delete a pointer a concurrent
    /// upsert just replaced. This is an embedded local store, so serializing the
    /// (rare, batched) writes costs nothing meaningful.
    mutation_lock: Arc<Mutex<()>>,
}

const WINDOW_VALUE_VERSION: u8 = 1;

fn encode_window(w: &toki_sync_protocol::WireWindow) -> Result<Vec<u8>> {
    let body = bincode::serialize(w)?;
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(WINDOW_VALUE_VERSION);
    out.extend_from_slice(&body);
    Ok(out)
}

enum WindowDecode {
    Valid(toki_sync_protocol::WireWindow),
    FutureVersion,
    Corrupt,
}

fn decode_window_versioned(bytes: &[u8]) -> WindowDecode {
    let Some((&version, body)) = bytes.split_first() else {
        return WindowDecode::Corrupt;
    };
    if version > WINDOW_VALUE_VERSION {
        return WindowDecode::FutureVersion;
    }
    match bincode::deserialize(body) {
        Ok(w) => WindowDecode::Valid(w),
        Err(_) => WindowDecode::Corrupt,
    }
}

fn decode_window(bytes: &[u8]) -> Option<toki_sync_protocol::WireWindow> {
    match decode_window_versioned(bytes) {
        WindowDecode::Valid(w) => Some(w),
        _ => None,
    }
}

/// Windows row key: user\0provider\0limit_id\0account\0[kind u8][window_end_ms i64 BE].
/// Excludes device_id by design — one row per account-level window.
fn window_row_key(user_id: &str, provider: &str, w: &toki_sync_protocol::WireWindow) -> Vec<u8> {
    let mut key = Vec::with_capacity(
        user_id.len() + provider.len() + w.limit_id.len() + w.account.len() + 4 + 9,
    );
    key.extend_from_slice(user_id.as_bytes());
    key.push(0);
    key.extend_from_slice(provider.as_bytes());
    key.push(0);
    key.extend_from_slice(w.limit_id.as_bytes());
    key.push(0);
    key.extend_from_slice(w.account.as_bytes());
    key.push(0);
    key.push(w.window_kind);
    key.extend_from_slice(&w.window_end_ms.to_be_bytes());
    key
}

impl FjallEventStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory for event store: {}", parent.display()))?;
        }

        let db = FjallDatabase::builder(path)
            .open()
            .context("open fjall event store")?;

        let opts = || KeyspaceCreateOptions::default();
        let meta = db.keyspace("meta", opts).context("open meta keyspace")?;
        let events = db.keyspace("events", opts).context("open events keyspace")?;
        let idx_msg = db.keyspace("idx_msg", opts).context("open idx_msg keyspace")?;
        let idx_user = db.keyspace("idx_user", opts).context("open idx_user keyspace")?;
        let windows = db.keyspace("windows", opts).context("open windows keyspace")?;

        // Check schema version — clear data if mismatched
        let stored = meta.get("schema_version").ok().flatten()
            .and_then(|b| String::from_utf8_lossy(&b).parse::<u32>().ok())
            .unwrap_or(0);

        if stored != 0 && stored != EVENT_SCHEMA_VERSION {
            tracing::warn!("Event store schema changed ({stored} -> {EVENT_SCHEMA_VERSION}), clearing data");
            drop(meta);
            drop(events);
            drop(idx_msg);
            drop(idx_user);
            // Kept alive here, this Arc handle into the old FjallDatabase
            // would outlive the remove_dir_all and the recursive reopen.
            drop(windows);
            drop(db);
            std::fs::remove_dir_all(path).ok();
            return Self::open(path); // recursive call to reopen fresh
        }
        meta.insert("schema_version", EVENT_SCHEMA_VERSION.to_string().as_bytes())?;

        // One-time backfill of the per-user index for stores created before it
        // existed. Guarded by a marker so it runs exactly once.
        //
        // The index entries are committed in bounded, idempotent chunks (each
        // (user_key -> value) insert is deterministic), and the completion
        // marker is written LAST in its own commit. A scan or deserialize error
        // aborts with no marker, so the whole pass re-runs on the next open
        // instead of marking a partial backfill "done". If `events` is empty
        // this just writes the marker.
        if meta.get("idx_user_backfilled").ok().flatten().is_none() {
            const BACKFILL_CHUNK: usize = 4096;
            let mut batch = db.batch();
            let mut in_batch = 0usize;
            let mut count = 0u64;
            for guard in events.range(Vec::<u8>::new()..) {
                let kv = guard.into_inner().context("idx_user backfill scan")?;
                let ev = bincode::deserialize::<ServerEvent>(&kv.1)
                    .context("idx_user backfill deserialize")?;
                let user_key = Self::user_idx_key(&ev.user_id, &kv.0);
                batch.insert(&idx_user, user_key, kv.1.to_vec());
                count += 1;
                in_batch += 1;
                if in_batch >= BACKFILL_CHUNK {
                    batch.commit().context("idx_user backfill chunk commit")?;
                    batch = db.batch();
                    in_batch = 0;
                }
            }
            if in_batch > 0 {
                batch.commit().context("idx_user backfill chunk commit")?;
            }
            // Marker last, so it is only set once every entry is durably indexed.
            let mut marker = db.batch();
            marker.insert(&meta, b"idx_user_backfilled".to_vec(), b"1".to_vec());
            marker.commit().context("idx_user backfill marker commit")?;
            if count > 0 {
                tracing::info!("backfilled per-user event index ({count} entries)");
            }
        }

        Ok(FjallEventStore {
            db,
            events,
            idx_msg,
            idx_user,
            windows,
            windows_lock: Arc::new(Mutex::new(())),
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Build the events keyspace key: [ts_ms(8 bytes BE)][device_id\0msg_id]
    fn event_key(ts_ms: i64, device_id: &str, msg_id: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(8 + device_id.len() + 1 + msg_id.len());
        key.extend_from_slice(&ts_ms.to_be_bytes());
        key.extend_from_slice(device_id.as_bytes());
        key.push(0);
        key.extend_from_slice(msg_id.as_bytes());
        key
    }

    /// Build the idx_msg key: [device_id\0provider\0msg_id]
    fn idx_key(device_id: &str, provider: &str, msg_id: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(device_id.len() + 1 + provider.len() + 1 + msg_id.len());
        key.extend_from_slice(device_id.as_bytes());
        key.push(0);
        key.extend_from_slice(provider.as_bytes());
        key.push(0);
        key.extend_from_slice(msg_id.as_bytes());
        key
    }

    /// Build the idx_user key: [user_id\0][event_key], where event_key is
    /// [ts_ms(8 BE)][device_id\0msg_id]. The `\0` after user_id delimits it so a
    /// prefix scan for one user can't spill into another whose id shares a prefix.
    fn user_idx_key(user_id: &str, event_key: &[u8]) -> Vec<u8> {
        let mut key = Vec::with_capacity(user_id.len() + 1 + event_key.len());
        key.extend_from_slice(user_id.as_bytes());
        key.push(0);
        key.extend_from_slice(event_key);
        key
    }
}

/// Upsert a single event within a batch (free function to avoid &self borrow issues).
fn upsert_one(
    events_ks: &Keyspace,
    idx_msg_ks: &Keyspace,
    idx_user_ks: &Keyspace,
    batch: &mut fjall::OwnedWriteBatch,
    event: &ServerEvent,
) {
    let idx_key = FjallEventStore::idx_key(&event.device_id, &event.provider, &event.msg_id);
    let new_event_key = FjallEventStore::event_key(event.ts_ms, &event.device_id, &event.msg_id);

    // Check if previous event exists for this (device_id, provider, msg_id)
    if let Ok(Some(prev_key)) = idx_msg_ks.get(&idx_key) {
        // Compare timestamps with the committed predecessor. ClickHouse's
        // ReplacingMergeTree(ts_ms) keeps the max-ts row, so an out-of-order
        // replay carrying an OLDER ts must not clobber the newer committed
        // event. The event key is [ts_ms(8 BE)][...], so read ts from its head.
        let prev_ts = if prev_key.len() >= 8 {
            i64::from_be_bytes(prev_key[..8].try_into().unwrap_or([0; 8]))
        } else {
            i64::MIN
        };
        if event.ts_ms < prev_ts {
            return;
        }

        // Remove the predecessor's per-user index entry using ITS OWN user_id,
        // not the incoming event's: a device can be re-registered to a different
        // user, in which case the inline copy still lives under the old user and
        // must be cleared there (using the incoming user_id would strand it).
        let prev_user_id = events_ks.get(&prev_key).ok().flatten()
            .and_then(|v| bincode::deserialize::<ServerEvent>(&v).ok())
            .map(|e| e.user_id)
            .unwrap_or_else(|| event.user_id.clone());
        let prev_user_key = FjallEventStore::user_idx_key(&prev_user_id, &prev_key);
        batch.remove(events_ks, prev_key.to_vec());
        batch.remove(idx_user_ks, prev_user_key);
    }

    // Insert new event + update both indexes
    let value = bincode::serialize(event).expect("ServerEvent serialize");
    let user_key = FjallEventStore::user_idx_key(&event.user_id, &new_event_key);
    batch.insert(events_ks, new_event_key.clone(), value.clone());
    batch.insert(idx_user_ks, user_key, value);
    batch.insert(idx_msg_ks, idx_key, new_event_key);
}

/// Iterate events in time range [since_ms, until_ms), applying user filter.
///
/// Uses half-open interval `[since_ms, until_ms)` which matches the final
/// result of `aggregate_events_to_toki_json` (which skips `ts_ms >= until_ms`).
/// Note: the local daemon's `for_each_event` uses `[since, until]` (inclusive),
/// but its aggregate function then filters with `ts_ms >= until_ms { continue }`,
/// producing the same effective `[since, until)` range.
fn scan_events(
    events_ks: &Keyspace,
    since_ms: i64,
    until_ms: i64,
    filter: &UserFilter,
) -> Vec<ServerEvent> {
    use std::collections::HashSet;

    // Pre-build HashSet for Multiple filter (O(1) lookup instead of O(n))
    let uid_set: Option<HashSet<&str>> = match filter {
        UserFilter::Multiple(uids) => Some(uids.iter().map(|s| s.as_str()).collect()),
        _ => None,
    };

    let start_key = since_ms.to_be_bytes().to_vec();
    let mut results = Vec::new();

    for guard in events_ks.range(start_key..) {
        let kv = match guard.into_inner() {
            Ok(kv) => kv,
            Err(_) => continue,
        };
        let key = &kv.0;
        if key.len() < 8 { continue; }

        let ts = i64::from_be_bytes(match key[..8].try_into() {
            Ok(b) => b,
            Err(_) => continue,
        });
        if ts >= until_ms { break; }

        let event: ServerEvent = match bincode::deserialize(&kv.1) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Apply user filter
        match filter {
            UserFilter::Single(uid) => {
                if event.user_id != *uid { continue; }
            }
            UserFilter::Multiple(_) => {
                if !uid_set.as_ref().unwrap().contains(event.user_id.as_str()) { continue; }
            }
            UserFilter::All => {}
        }

        results.push(event);
    }

    results
}

/// Iterate one user's events in [since_ms, until_ms) via the per-user index.
///
/// Scans only the `[user_id\0]` prefix of `idx_user`, whose keys sort by ts_ms
/// (the 8 BE bytes right after the delimiter), so we can break once past
/// `until_ms`. Each index value is an inline copy of the event.
fn scan_user_events(
    idx_user_ks: &Keyspace,
    user_id: &str,
    since_ms: i64,
    until_ms: i64,
) -> Vec<ServerEvent> {
    let prefix = {
        let mut p = user_id.as_bytes().to_vec();
        p.push(0);
        p
    };
    let ts_off = prefix.len();
    let mut results = Vec::new();

    for guard in idx_user_ks.prefix(&prefix) {
        let kv = match guard.into_inner() {
            Ok(kv) => kv,
            Err(_) => continue,
        };
        let key = &kv.0;
        // Key is [user_id\0][ts_ms(8 BE)][...]; extract ts_ms after the prefix.
        if key.len() < ts_off + 8 { continue; }
        let ts = i64::from_be_bytes(match key[ts_off..ts_off + 8].try_into() {
            Ok(b) => b,
            Err(_) => continue,
        });
        if ts >= until_ms { break; }
        if ts < since_ms { continue; }

        match bincode::deserialize::<ServerEvent>(&kv.1) {
            Ok(e) => results.push(e),
            Err(_) => continue,
        }
    }

    results
}

#[async_trait::async_trait]
impl EventStore for FjallEventStore {
    async fn upsert_events(&self, events: &[ServerEvent]) -> Result<()> {
        if events.is_empty() { return Ok(()); }

        let db = self.db.clone();
        let events_ks = self.events.clone();
        let idx_msg_ks = self.idx_msg.clone();
        let idx_user_ks = self.idx_user.clone();
        let mutation_lock = self.mutation_lock.clone();
        let events = events.to_vec();

        tokio::task::spawn_blocking(move || {
            use std::collections::HashMap;

            // Serialize read-then-write against concurrent upsert/cleanup/delete.
            let _guard = mutation_lock.lock().unwrap_or_else(|e| e.into_inner());

            // Pre-dedup the batch in memory by idx_key. `upsert_one` looks up
            // the previous event via the *committed* idx_msg index, so it cannot
            // see pending inserts from the same batch: two items sharing an
            // idx_key (same device/provider/bare_msg_id) but different ts_ms would
            // otherwise each land as a distinct event row (event_key includes
            // ts_ms) with idx_msg pointing only to the last — inflating usage.
            // Keep the max-ts item per idx_key, matching ClickHouse's
            // ReplacingMergeTree(ts_ms) semantics.
            let mut winner: HashMap<Vec<u8>, &ServerEvent> = HashMap::with_capacity(events.len());
            for event in &events {
                let idx_key = FjallEventStore::idx_key(&event.device_id, &event.provider, &event.msg_id);
                match winner.get(&idx_key) {
                    Some(existing) if existing.ts_ms >= event.ts_ms => {}
                    _ => { winner.insert(idx_key, event); }
                }
            }

            let mut batch = db.batch();
            for event in winner.values() {
                upsert_one(&events_ks, &idx_msg_ks, &idx_user_ks, &mut batch, event);
            }
            batch.commit().context("fjall batch commit")?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn query_events(
        &self,
        since_ms: i64,
        until_ms: i64,
        filter: UserFilter,
    ) -> Result<Vec<ServerEvent>> {
        let events_ks = self.events.clone();
        let idx_user_ks = self.idx_user.clone();

        tokio::task::spawn_blocking(move || {
            // User-scoped queries use the per-user index so they touch only the
            // relevant users' events. `scope=all` still scans the full events
            // keyspace (no user narrowing possible).
            let results = match &filter {
                UserFilter::Single(uid) => {
                    scan_user_events(&idx_user_ks, uid, since_ms, until_ms)
                }
                UserFilter::Multiple(uids) => {
                    let mut out = Vec::new();
                    for uid in uids {
                        out.extend(scan_user_events(&idx_user_ks, uid, since_ms, until_ms));
                    }
                    out
                }
                UserFilter::All => scan_events(&events_ks, since_ms, until_ms, &filter),
            };
            Ok(results)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn cleanup_old_dedup(&self, device_id: &str, cutoff_ms: i64) -> Result<()> {
        let db = self.db.clone();
        let idx_msg_ks = self.idx_msg.clone();
        let mutation_lock = self.mutation_lock.clone();
        let device_id = device_id.to_string();

        tokio::task::spawn_blocking(move || {
            // Serialize against upsert so cleanup never prunes a pointer that a
            // concurrent replacement just installed.
            let _guard = mutation_lock.lock().unwrap_or_else(|e| e.into_inner());

            let prefix = {
                let mut p = device_id.as_bytes().to_vec();
                p.push(0);
                p
            };

            let mut batch = db.batch();
            let mut count = 0u64;

            for guard in idx_msg_ks.prefix(&prefix) {
                let kv = match guard.into_inner() {
                    Ok(kv) => kv,
                    Err(_) => continue,
                };
                // Value is the event key: [ts_ms(8 bytes)][rest]
                if kv.1.len() >= 8 {
                    let ts = i64::from_be_bytes(
                        kv.1[..8].try_into().unwrap_or([0; 8])
                    );
                    if ts < cutoff_ms {
                        // Only remove the idx_msg entry — events data is preserved
                        // for historical queries. Without idx_msg, an old event
                        // won't be dedup'd if the same msg_id arrives again. The
                        // caller sizes cutoff_ms from a generous retention window
                        // (default 30 days) so late corrections still dedup.
                        batch.remove(&idx_msg_ks, kv.0.to_vec());
                        count += 1;
                    }
                }
            }

            if count > 0 {
                batch.commit().context("fjall cleanup_old_dedup commit")?;
                tracing::info!("cleaned up {count} old idx_msg entries for device {device_id}");
            }
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn delete_device_events(&self, device_id: &str) -> Result<()> {
        let db = self.db.clone();
        let events_ks = self.events.clone();
        let idx_msg_ks = self.idx_msg.clone();
        let idx_user_ks = self.idx_user.clone();
        let mutation_lock = self.mutation_lock.clone();
        let device_id = device_id.to_string();

        tokio::task::spawn_blocking(move || {
            let _guard = mutation_lock.lock().unwrap_or_else(|e| e.into_inner());

            let mut batch = db.batch();

            // Full scan of the events keyspace, matching by the device_id stored
            // in each event value. We can't prefix-scan `events` (its key is
            // [ts_ms][device_id\0msg_id], so device_id is not the prefix), and an
            // idx_msg-only enumeration would miss events whose dedup pointer was
            // already pruned by cleanup_old_dedup (>retention old) — those would
            // survive device deletion as orphans. Deletion is a rare admin op, so
            // the full scan is acceptable. For each matching event we drop the
            // event row, its inline per-user index entry, and its idx_msg pointer.
            for guard in events_ks.range(Vec::<u8>::new()..) {
                let kv = match guard.into_inner() {
                    Ok(kv) => kv,
                    Err(_) => continue,
                };
                let ev = match bincode::deserialize::<ServerEvent>(&kv.1) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if ev.device_id != device_id { continue; }

                batch.remove(&idx_user_ks, FjallEventStore::user_idx_key(&ev.user_id, &kv.0));
                batch.remove(&idx_msg_ks, FjallEventStore::idx_key(&ev.device_id, &ev.provider, &ev.msg_id));
                batch.remove(&events_ks, kv.0.to_vec());
            }

            // Sweep any residual idx_msg pointers for this device whose events
            // were already gone (defensive — keeps the dedup index consistent).
            let prefix = {
                let mut p = device_id.as_bytes().to_vec();
                p.push(0);
                p
            };
            for guard in idx_msg_ks.prefix(&prefix) {
                if let Ok(kv) = guard.into_inner() {
                    batch.remove(&idx_msg_ks, kv.0.to_vec());
                }
            }

            batch.commit().context("fjall delete_device commit")?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn upsert_windows(
        &self,
        user_id: &str,
        provider: &str,
        items: &[toki_sync_protocol::WireWindow],
    ) -> Result<usize> {
        if items.is_empty() { return Ok(0); }
        let windows_ks = self.windows.clone();
        let windows_lock = self.windows_lock.clone();
        let user_id = user_id.to_string();
        let provider = provider.to_string();
        let items = items.to_vec();
        tokio::task::spawn_blocking(move || {
            let _guard = windows_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut skipped = 0usize;
            for w in &items {
                let key = window_row_key(&user_id, &provider, w);
                let merged = match windows_ks.get(&key)? {
                    Some(prev_bytes) => match decode_window_versioned(&prev_bytes) {
                        WindowDecode::Valid(mut prev) => {
                            let before = prev.clone();
                            super::merge_wire_windows(&mut prev, w);
                            // Cursorless resends carry mostly-unchanged rows:
                            // skip the write when the merge changed nothing.
                            if prev == before {
                                continue;
                            }
                            prev
                        }
                        // Future value version: preserve, never clobber — and
                        // report it as not-stored so the caller doesn't ack a
                        // fully-accepted batch.
                        WindowDecode::FutureVersion => {
                            skipped += 1;
                            continue;
                        }
                        // Corrupt current-version bytes: recover with the
                        // incoming snapshot instead of preserving corruption.
                        WindowDecode::Corrupt => {
                            tracing::warn!("corrupt window value replaced");
                            w.clone()
                        }
                    },
                    None => w.clone(),
                };
                windows_ks.insert(&key, encode_window(&merged)?)?;
            }
            Ok(skipped)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn query_user_windows(
        &self,
        user_id: &str,
        since_ms: i64,
    ) -> Result<Vec<(String, toki_sync_protocol::WireWindow)>> {
        let windows_ks = self.windows.clone();
        let prefix = format!("{}\0", user_id).into_bytes();
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for guard in windows_ks.prefix(&prefix) {
                let kv = guard.into_inner()?;
                // key = user\0provider\0limit\0account\0[kind][end]
                let rest = &kv.0[prefix.len()..];
                let Some(sep) = rest.iter().position(|&b| b == 0) else { continue };
                // Filter on the key's trailing 8 bytes (the anchor) BEFORE
                // decoding: storage holds up to 730 days and a dashboard asks
                // for 7-30, so decoding first meant a bincode deserialize and
                // three String allocations per row, ~98% of them discarded.
                // Same trick cleanup_old_windows uses.
                let anchor = kv.0.len().checked_sub(8)
                    .and_then(|i| kv.0.get(i..))
                    .and_then(|b| b.try_into().ok().map(i64::from_be_bytes));
                if anchor.map(|a| a < since_ms).unwrap_or(false) {
                    continue;
                }
                let provider = String::from_utf8_lossy(&rest[..sep]).to_string();
                if let Some(w) = decode_window(&kv.1) {
                    if w.window_end_ms >= since_ms {
                        out.push((provider, w));
                    }
                }
            }
            Ok(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn cleanup_old_windows(&self, cutoff_ms: i64) -> Result<usize> {
        let windows_ks = self.windows.clone();
        let mutation_lock = self.windows_lock.clone();
        tokio::task::spawn_blocking(move || {
            // Scan WITHOUT the lock (reads are safe); take it only for the
            // removals — holding it across a multi-million-row scan would
            // stall every window upsert for the scan's duration.
            let mut stale = Vec::new();
            for guard in windows_ks.iter() {
                let kv = guard.into_inner()?;
                // The anchor is the key's trailing 8 bytes — no value decode.
                let anchor = kv.0.len().checked_sub(8)
                    .and_then(|i| kv.0.get(i..))
                    .and_then(|b| b.try_into().ok().map(i64::from_be_bytes));
                if let Some(a) = anchor {
                    if a < cutoff_ms {
                        stale.push(kv.0.to_vec());
                    }
                }
            }
            let n = stale.len();
            {
                let _guard = mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
                for key in stale {
                    windows_ks.remove(key)?;
                }
            }
            Ok(n)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn delete_user_windows(&self, user_id: &str) -> Result<()> {
        let windows_ks = self.windows.clone();
        let mutation_lock = self.windows_lock.clone();
        let prefix = format!("{}\0", user_id).into_bytes();
        tokio::task::spawn_blocking(move || {
            let _guard = mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut stale = Vec::new();
            for guard in windows_ks.prefix(&prefix) {
                let kv = guard.into_inner()?;
                stale.push(kv.0.to_vec());
            }
            for key in stale {
                windows_ks.remove(key)?;
            }
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_event(device: &str, msg: &str, ts: i64, model: &str, input: u64) -> ServerEvent {
        ServerEvent {
            device_id: device.to_string(),
            user_id: "user1".to_string(),
            msg_id: msg.to_string(),
            ts_ms: ts,
            provider: "claude_code".to_string(),
            model: model.to_string(),
            project: "test".to_string(),
            input_tokens: input,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            usage_total: input,
        }
    }

    #[tokio::test]
    async fn test_upsert_dedup() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let e1 = make_event("d1", "msg_abc", 1000, "opus", 8);
        store.upsert_events(&[e1]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 8);

        // Upsert same msg_id — should replace
        let e2 = make_event("d1", "msg_abc", 2000, "opus", 246);
        store.upsert_events(&[e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 246);
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_batch_internal_dedup_same_msg_id() {
        // Two items in the SAME batch share a bare msg_id but differ on ts_ms
        // (the daemon normalizes `abc:1`/`abc:2` -> `abc`). Only the later
        // (max-ts) item must survive — otherwise the Fjall backend produces
        // duplicate usage that ClickHouse (ReplacingMergeTree) does not.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let e1 = make_event("d1", "msg_abc", 1000, "opus", 8);
        let e2 = make_event("d1", "msg_abc", 2000, "opus", 246);
        store.upsert_events(&[e1, e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1, "same-batch same-msg_id must collapse to one row");
        assert_eq!(events[0].input_tokens, 246);
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_batch_distinct_codex_events_preserved() {
        // Codex (toki >= v2.1.1) uses a per-event xxh3 hash as the bare msg_id,
        // so two codex events in one batch have DIFFERENT (device, provider,
        // msg_id) idx_keys and must BOTH survive. The in-batch pre-dedup keys on
        // the full idx_key, so it must never collapse them.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e1 = make_event("d1", "hash_aaaa", 1000, "gpt-5", 100);
        e1.provider = "codex".to_string();
        let mut e2 = make_event("d1", "hash_bbbb", 1000, "gpt-5", 200);
        e2.provider = "codex".to_string();
        store.upsert_events(&[e1, e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 2, "distinct codex hashes must not be collapsed");
    }

    #[tokio::test]
    async fn test_batch_internal_dedup_reversed_order() {
        // Max-ts wins regardless of the order items appear in the batch.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let later = make_event("d1", "msg_abc", 2000, "opus", 246);
        let earlier = make_event("d1", "msg_abc", 1000, "opus", 8);
        store.upsert_events(&[later, earlier]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 246);
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_different_msg_ids() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let e1 = make_event("d1", "msg_a", 1000, "opus", 100);
        let e2 = make_event("d1", "msg_b", 2000, "opus", 200);
        store.upsert_events(&[e1, e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn test_time_range_filter() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        store.upsert_events(&[
            make_event("d1", "a", 1000, "opus", 100),
            make_event("d1", "b", 2000, "opus", 200),
            make_event("d1", "c", 3000, "opus", 300),
        ]).await.unwrap();

        let events = store.query_events(1500, 2500, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 200);
    }

    #[tokio::test]
    async fn test_user_filter() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e1 = make_event("d1", "a", 1000, "opus", 100);
        e1.user_id = "alice".to_string();
        let mut e2 = make_event("d2", "b", 2000, "opus", 200);
        e2.user_id = "bob".to_string();
        store.upsert_events(&[e1, e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 100);
    }

    #[tokio::test]
    async fn test_user_index_after_dedup_replacement() {
        // A dedup replacement must move the per-user index entry to the new
        // event, not leave a stale one behind (which would double-count).
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e1 = make_event("d1", "msg_x", 1000, "opus", 100);
        e1.user_id = "alice".to_string();
        store.upsert_events(&[e1]).await.unwrap();

        let mut e2 = make_event("d1", "msg_x", 2000, "opus", 250);
        e2.user_id = "alice".to_string();
        store.upsert_events(&[e2]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert_eq!(events.len(), 1, "per-user index must not retain the replaced event");
        assert_eq!(events[0].input_tokens, 250);
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_user_index_delete_device() {
        // Deleting a device must also purge its per-user index entries.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e1 = make_event("d1", "a", 1000, "opus", 100);
        e1.user_id = "alice".to_string();
        let mut e2 = make_event("d2", "b", 2000, "opus", 200);
        e2.user_id = "alice".to_string();
        store.upsert_events(&[e1, e2]).await.unwrap();

        store.delete_device_events("d1").await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].device_id, "d2");
    }

    #[tokio::test]
    async fn test_user_index_backfill() {
        // Simulate a store written before the per-user index existed: an event
        // sits in `events` with no idx_user entry and no backfill marker.
        // Opening via FjallEventStore must backfill so user queries work.
        let dir = TempDir::new().unwrap();
        let ev = {
            let mut e = make_event("d1", "m", 1000, "opus", 42);
            e.user_id = "alice".to_string();
            e
        };
        {
            let db = fjall::Database::builder(dir.path()).open().unwrap();
            let events = db.keyspace("events", || fjall::KeyspaceCreateOptions::default()).unwrap();
            let key = FjallEventStore::event_key(ev.ts_ms, &ev.device_id, &ev.msg_id);
            let val = bincode::serialize(&ev).unwrap();
            events.insert(key, val).unwrap();
        }

        let store = FjallEventStore::open(dir.path()).unwrap();
        let events = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert_eq!(events.len(), 1, "backfill must index pre-existing events");
        assert_eq!(events[0].input_tokens, 42);
    }

    #[tokio::test]
    async fn test_delete_device() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        store.upsert_events(&[
            make_event("d1", "a", 1000, "opus", 100),
            make_event("d2", "b", 2000, "opus", 200),
        ]).await.unwrap();

        store.delete_device_events("d1").await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].device_id, "d2");
    }

    #[tokio::test]
    async fn test_concurrent_upsert_no_double_count() {
        // Two concurrent upserts of the SAME (device, provider, msg_id) but
        // different ts must not both land as rows. The internal mutation lock
        // serializes the read-then-write, and the max-ts guard makes the winner
        // deterministic regardless of which commits first.
        let dir = TempDir::new().unwrap();
        let store = std::sync::Arc::new(FjallEventStore::open(dir.path()).unwrap());

        let s1 = store.clone();
        let s2 = store.clone();
        let h1 = tokio::spawn(async move {
            s1.upsert_events(&[make_event("d1", "msg", 1000, "opus", 8)]).await
        });
        let h2 = tokio::spawn(async move {
            s2.upsert_events(&[make_event("d1", "msg", 2000, "opus", 246)]).await
        });
        h1.await.unwrap().unwrap();
        h2.await.unwrap().unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1, "concurrent upserts must not double-count");
        assert_eq!(events[0].input_tokens, 246, "max-ts event must win");
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_older_ts_replay_does_not_clobber() {
        // A later call carrying an OLDER ts for the same tuple must NOT replace
        // the newer committed event (matches ClickHouse ReplacingMergeTree(ts_ms)).
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        store.upsert_events(&[make_event("d1", "m", 2000, "opus", 246)]).await.unwrap();
        store.upsert_events(&[make_event("d1", "m", 1000, "opus", 8)]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 246, "older replay must not clobber newer event");
        assert_eq!(events[0].ts_ms, 2000);
    }

    #[tokio::test]
    async fn test_device_reassigned_to_new_user_clears_old_index() {
        // Same device/provider/msg_id re-registered under a different user: the
        // predecessor's inline per-user index entry must be removed under the OLD
        // user, not the incoming one, or the old user keeps a phantom event.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e1 = make_event("d1", "x", 1000, "opus", 100);
        e1.user_id = "alice".to_string();
        store.upsert_events(&[e1]).await.unwrap();

        let mut e2 = make_event("d1", "x", 2000, "opus", 250);
        e2.user_id = "bob".to_string();
        store.upsert_events(&[e2]).await.unwrap();

        let alice = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert!(alice.is_empty(), "old user's inline index entry must be cleared on reassignment");
        let bob = store.query_events(0, i64::MAX, UserFilter::Single("bob".into())).await.unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].input_tokens, 250);
    }

    #[tokio::test]
    async fn test_delete_device_after_dedup_cleanup() {
        // An event whose dedup pointer was pruned by cleanup_old_dedup must still
        // be removed on device deletion (the full-scan path), not orphaned.
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        let mut e = make_event("d1", "m", 1000, "opus", 100);
        e.user_id = "alice".to_string();
        store.upsert_events(&[e]).await.unwrap();

        // Prune the dedup pointer as the retention cleanup would (cutoff > ts).
        store.cleanup_old_dedup("d1", 2000).await.unwrap();

        store.delete_device_events("d1").await.unwrap();

        let all = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert!(all.is_empty(), "event orphaned by dedup cleanup must still be deleted");
        let alice = store.query_events(0, i64::MAX, UserFilter::Single("alice".into())).await.unwrap();
        assert!(alice.is_empty(), "per-user index entry must be cleared too");
    }

    #[tokio::test]
    async fn test_backfill_aborts_on_corrupt_value() {
        // A value that isn't a ServerEvent must abort the backfill with no marker
        // set, so the next open retries instead of marking a partial backfill done.
        let dir = TempDir::new().unwrap();
        {
            let db = fjall::Database::builder(dir.path()).open().unwrap();
            let events = db.keyspace("events", || fjall::KeyspaceCreateOptions::default()).unwrap();
            let key = FjallEventStore::event_key(1000, "d1", "m");
            events.insert(key, b"not-a-serverevent".to_vec()).unwrap();
        }

        assert!(FjallEventStore::open(dir.path()).is_err(), "corrupt event must abort backfill");

        let db = fjall::Database::builder(dir.path()).open().unwrap();
        let meta = db.keyspace("meta", || fjall::KeyspaceCreateOptions::default()).unwrap();
        assert!(
            meta.get("idx_user_backfilled").ok().flatten().is_none(),
            "marker must remain unset after a failed backfill"
        );
    }

    #[tokio::test]
    async fn test_cross_device_dedup_isolation() {
        let dir = TempDir::new().unwrap();
        let store = FjallEventStore::open(dir.path()).unwrap();

        // Same msg_id, different devices — should NOT dedup each other
        store.upsert_events(&[
            make_event("d1", "msg_same", 1000, "opus", 100),
            make_event("d2", "msg_same", 2000, "opus", 200),
        ]).await.unwrap();

        let events = store.query_events(0, i64::MAX, UserFilter::All).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn windows_merge_is_field_wise_and_user_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallEventStore::open(&dir.path().join("ev")).unwrap();

        let mut w = toki_sync_protocol::WireWindow {
            window_kind: 0,
            limit_id: "five_hour".into(),
            account: "acct".into(),
            window_end_ms: 1_786_000_000_000,
            raw_resets_at_ms: 1_786_000_010_000,
            window_minutes: 300,
            peak_pct_x100: 9000,
            last_pct_x100: 9000,
            observed_ts_ms: 100,
            first_seen_ms: 50,
            finalized: false,
            maxed_out: false,
            limit_reached_kind: 0,
            time_to_100_ms: -1,
            active_ms: 500,
            last_sample_gap_ms: 10_000,
            sampled_active_fraction: 1000,
            n_samples: 10,
            plan: "old".into(),
        };
        store.upsert_windows("user-a", "claude_code", &[w.clone()]).await.unwrap();

        // Device B saw a lower peak but observed later and finalized.
        w.peak_pct_x100 = 4000;
        w.observed_ts_ms = 200;
        w.finalized = true;
        w.maxed_out = true;
        w.time_to_100_ms = 7_200_000;
        w.plan = "new".into();
        store.upsert_windows("user-a", "claude_code", &[w.clone()]).await.unwrap();

        let rows = store.query_user_windows("user-a", 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        let (provider, merged) = &rows[0];
        assert_eq!(provider, "claude_code");
        assert_eq!(merged.peak_pct_x100, 9000); // max, not last writer
        assert_eq!(merged.observed_ts_ms, 200);
        assert!(merged.finalized && merged.maxed_out);
        assert_eq!(merged.time_to_100_ms, 7_200_000);
        assert_eq!(merged.plan, "new");

        // Another user's windows are invisible and separately deletable.
        store.upsert_windows("user-b", "codex", &[w.clone()]).await.unwrap();
        assert_eq!(store.query_user_windows("user-b", 0).await.unwrap().len(), 1);
        store.delete_user_windows("user-a").await.unwrap();
        assert_eq!(store.query_user_windows("user-a", 0).await.unwrap().len(), 0);
        assert_eq!(store.query_user_windows("user-b", 0).await.unwrap().len(), 1);

        // since filter
        assert_eq!(store.query_user_windows("user-b", 1_790_000_000_000).await.unwrap().len(), 0);
    }

}
