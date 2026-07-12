use std::path::Path;

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
            drop(db);
            std::fs::remove_dir_all(path).ok();
            return Self::open(path); // recursive call to reopen fresh
        }
        meta.insert("schema_version", EVENT_SCHEMA_VERSION.to_string().as_bytes())?;

        // One-time backfill of the per-user index for stores created before it
        // existed. Guarded by a marker so it runs exactly once. If `events` is
        // empty this just writes the marker.
        if meta.get("idx_user_backfilled").ok().flatten().is_none() {
            let mut batch = db.batch();
            let mut count = 0u64;
            for guard in events.range(Vec::<u8>::new()..) {
                let kv = match guard.into_inner() {
                    Ok(kv) => kv,
                    Err(_) => continue,
                };
                if let Ok(ev) = bincode::deserialize::<ServerEvent>(&kv.1) {
                    let user_key = Self::user_idx_key(&ev.user_id, &kv.0);
                    batch.insert(&idx_user, user_key, kv.1.to_vec());
                    count += 1;
                }
            }
            batch.insert(&meta, b"idx_user_backfilled".to_vec(), b"1".to_vec());
            batch.commit().context("idx_user backfill commit")?;
            if count > 0 {
                tracing::info!("backfilled per-user event index ({count} entries)");
            }
        }

        Ok(FjallEventStore { db, events, idx_msg, idx_user })
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
    let value = bincode::serialize(event).expect("ServerEvent serialize");

    // Check if previous event exists for this (device_id, provider, msg_id)
    if let Ok(Some(prev_key)) = idx_msg_ks.get(&idx_key) {
        // Delete old event from events keyspace and its per-user index entry.
        // The device (and hence user) is stable across a dedup replacement.
        let prev_user_key = FjallEventStore::user_idx_key(&event.user_id, &prev_key);
        batch.remove(events_ks, prev_key.to_vec());
        batch.remove(idx_user_ks, prev_user_key);
    }

    // Insert new event + update both indexes
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
        let events = events.to_vec();

        tokio::task::spawn_blocking(move || {
            use std::collections::HashMap;

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
        let device_id = device_id.to_string();

        tokio::task::spawn_blocking(move || {
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
        let device_id = device_id.to_string();

        tokio::task::spawn_blocking(move || {
            let mut batch = db.batch();

            // Scan idx_msg for entries with this device_id prefix
            let prefix = {
                let mut p = device_id.as_bytes().to_vec();
                p.push(0);
                p
            };

            let mut idx_keys_to_delete = Vec::new();
            for guard in idx_msg_ks.prefix(&prefix) {
                let kv = match guard.into_inner() {
                    Ok(kv) => kv,
                    Err(_) => continue,
                };
                let event_key = &kv.1;
                // The per-user index key needs the user_id, which lives only in
                // the event value — read it back before removing the event row.
                if let Ok(Some(val)) = events_ks.get(event_key) {
                    if let Ok(ev) = bincode::deserialize::<ServerEvent>(&val) {
                        batch.remove(&idx_user_ks, FjallEventStore::user_idx_key(&ev.user_id, event_key));
                    }
                }
                batch.remove(&events_ks, event_key.to_vec());
                idx_keys_to_delete.push(kv.0.to_vec());
            }

            for key in idx_keys_to_delete {
                batch.remove(&idx_msg_ks, key);
            }

            batch.commit().context("fjall delete_device commit")?;
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
}
