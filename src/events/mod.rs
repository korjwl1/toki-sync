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
/// - `FjallEventStore`: embedded, standalone, msg_id-based dedup via idx_msg
/// - `ClickHouseEventStore`: external, ReplacingMergeTree auto-dedup
///
/// Backend is chosen at startup via config and cannot be switched at runtime.
#[async_trait::async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Insert or update events. Deduplicates by (device_id, msg_id):
    /// if an event with the same (device_id, msg_id) already exists,
    /// it is replaced with the new values.
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
}
