pub mod fjall_store;
pub mod clickhouse;

use anyhow::Result;

/// A fully-resolved event stored on the server.
///
/// Unlike the local daemon's StoredEvent (which uses dict-compressed u32 IDs),
/// this stores resolved strings because dict IDs are session-scoped and would
/// conflict across devices.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerEvent {
    pub device_id: String,
    pub user_id: String,
    /// Bare message ID (without timestamp suffix). Used as dedup key.
    pub msg_id: String,
    pub ts_ms: i64,
    pub provider: String,
    pub model: String,
    pub project: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Pre-computed usage total. For Claude: all 4 token types.
    /// For Codex: input + output only (cached_input ⊂ input, reasoning_output ⊂ output).
    #[serde(default)]
    pub usage_total: u64,
}

/// Scope filter for queries.
pub enum UserFilter {
    /// Single user (scope=self)
    Single(String),
    /// Multiple users (scope=team)
    Multiple(Vec<String>),
    /// All users (scope=all, admin only)
    All,
}

/// Event storage backend.
///
/// Two implementations:
/// - `FjallEventStore`: embedded LSM-tree, standalone mode.
///   Dedup via idx_msg secondary index (same pattern as local daemon).
/// - `ClickHouseEventStore`: external columnar DB.
///   Dedup via ReplacingMergeTree(ts_ms) ORDER BY (device_id, provider, msg_id).
///
/// Backend is chosen at startup via config (`events.backend`) and cannot
/// be switched at runtime. Data is NOT migrated between backends.
///
/// Key invariant: upsert_events is idempotent by (device_id, provider, msg_id).
/// Re-sending the same event (e.g., after crash recovery) produces the same result.
#[async_trait::async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Insert or update events. Deduplicates by (device_id, provider, msg_id):
    /// if an event with the same key already exists, it is replaced.
    async fn upsert_events(&self, events: &[ServerEvent]) -> Result<()>;

    /// Query events in [since_ms, until_ms) matching the user filter.
    async fn query_events(
        &self,
        since_ms: i64,
        until_ms: i64,
        filter: UserFilter,
    ) -> Result<Vec<ServerEvent>>;

    /// Delete all events for a specific device (used on schema mismatch reset).
    async fn delete_device_events(&self, device_id: &str) -> Result<()>;

    /// Clean up old dedup index entries for a device.
    /// Removes idx_msg entries whose event timestamp is older than cutoff_ms.
    async fn cleanup_old_dedup(&self, device_id: &str, cutoff_ms: i64) -> Result<()>;

    // ── Rate-limit windows (capability sync_windows_v1) ─────────────────────
    //
    // Windows are account-level provider state: the key deliberately excludes
    // device_id so every device's observation of the same window converges on
    // one row, merged FIELD-WISE (peak=max, flags=OR, first_seen=min — never
    // whole-row last-writer-wins, which would let a late-but-stale device
    // erase another device's higher peak). Clients resend their full recent
    // set; the merge makes that idempotent.

    /// Merge window snapshots for (user, provider). Keyed by
    /// (user_id, provider, limit_id, account, window_kind, window_end_ms).
    async fn upsert_windows(
        &self,
        user_id: &str,
        provider: &str,
        items: &[toki_sync_protocol::WireWindow],
    ) -> Result<()>;

    /// Windows whose anchor is >= since_ms for the given user, as
    /// (provider, window) pairs. v1 scope: single user only (scope=self).
    async fn query_user_windows(
        &self,
        user_id: &str,
        since_ms: i64,
    ) -> Result<Vec<(String, toki_sync_protocol::WireWindow)>>;

    /// Delete all window rows for a user (wired into account deletion —
    /// windows have no device_id, so device purges can never reach them).
    async fn delete_user_windows(&self, user_id: &str) -> Result<()>;
}

/// Field-wise merge shared by both backends (mirrors toki's local
/// WindowSnapshotV1::merge_from — keep the two in semantic lockstep).
pub fn merge_wire_windows(
    prev: &mut toki_sync_protocol::WireWindow,
    other: &toki_sync_protocol::WireWindow,
) {
    prev.peak_pct_x100 = prev.peak_pct_x100.max(other.peak_pct_x100);
    if other.observed_ts_ms > prev.observed_ts_ms {
        prev.observed_ts_ms = other.observed_ts_ms;
        prev.raw_resets_at_ms = other.raw_resets_at_ms;
        prev.last_sample_gap_ms = other.last_sample_gap_ms;
        prev.plan = other.plan.clone();
    }
    if other.first_seen_ms > 0 {
        prev.first_seen_ms = if prev.first_seen_ms > 0 {
            prev.first_seen_ms.min(other.first_seen_ms)
        } else {
            other.first_seen_ms
        };
    }
    prev.finalized |= other.finalized;
    prev.maxed_out |= other.maxed_out;
    prev.limit_reached_kind = prev.limit_reached_kind.max(other.limit_reached_kind);
    prev.time_to_100_ms = match (prev.time_to_100_ms, other.time_to_100_ms) {
        (-1, t) | (t, -1) => t,
        (a, b) => a.min(b),
    };
    prev.active_ms = prev.active_ms.max(other.active_ms);
    prev.sampled_active_fraction = prev.sampled_active_fraction.max(other.sampled_active_fraction);
    prev.n_samples = prev.n_samples.max(other.n_samples);
}
