//! Shared model types used by all database backends.

#[allow(dead_code)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub oidc_sub: Option<String>,
    pub oidc_issuer: Option<String>,
    pub active: bool,
}

pub struct NewUser {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
}

pub struct NewOidcUser {
    pub id: String,
    pub username: String,
    pub role: String,
    pub oidc_sub: String,
    pub oidc_issuer: String,
}

pub struct UserSummary {
    pub id: String,
    pub username: String,
    pub role: String,
    pub created_at: i64,
    pub active: bool,
}

pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub device_key: String,
    pub last_seen_at: i64,
}

pub struct DeviceAdminSummary {
    pub id: String,
    pub name: String,
    pub username: String,
    pub last_seen_at: i64,
}

#[allow(dead_code)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct TeamWithCount {
    pub id: String,
    pub name: String,
    pub member_count: i64,
    pub created_at: i64,
}

pub struct TeamMembership {
    pub team_id: String,
    pub team_name: String,
    pub role: String,
}

pub struct TeamMemberSummary {
    pub user_id: String,
    pub username: String,
    pub role: String,
    pub joined_at: i64,
}

pub struct PendingRegistration {
    pub id: String,
    pub username: String,
    pub requested_at: i64,
}

#[allow(dead_code)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub expires_at: i64,
    pub approved_by: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
}

// ─── Monitor settings ───────────────────────────────────────────────────────
//
// The opt-in monitor configuration channel. Deliberately separate from the sync
// protocol: `toki_sync_protocol` carries toki's collected usage data and only
// that. These rows are opaque to the server — it stores what the monitor sends
// and hands the same bytes back, so there is no second, drifting server-side
// idea of what a dashboard is.

/// One stored monitor entry, payload included.
pub struct MonitorSetting {
    pub key: String,
    /// Opaque payload. Never parsed by the server.
    pub value: String,
    pub version: i64,
    pub updated_at: i64,
}

/// One stored monitor entry with the payload left behind, for clients deciding
/// what is worth fetching.
pub struct MonitorSettingMeta {
    pub key: String,
    pub version: i64,
    pub updated_at: i64,
    pub size_bytes: i64,
}

/// Per-user storage ceiling. Enforced inside the write transaction so two
/// concurrent writes cannot both pass a check and land over the limit.
#[derive(Clone, Copy)]
pub struct MonitorQuota {
    pub max_entries: i64,
    pub max_total_bytes: i64,
}

/// Totals backing the quota, reported to clients so they can see headroom.
pub struct MonitorUsage {
    pub entries: i64,
    pub total_bytes: i64,
}

/// What a monitor-setting write actually did.
pub enum MonitorWriteOutcome {
    Written {
        version: i64,
        updated_at: i64,
        /// Version this write replaced; `None` when the entry was created.
        previous_version: Option<i64>,
    },
    /// A conditional write whose `if_version` did not match the stored row.
    /// `current_*` is `None` when no entry exists under that key.
    VersionMismatch {
        current_version: Option<i64>,
        current_updated_at: Option<i64>,
    },
    /// The write would have pushed the user over `MonitorQuota`.
    QuotaExceeded {
        what: &'static str,
        used: i64,
        limit: i64,
    },
}

/// What a monitor-setting delete actually did. Deletes support the same CAS
/// rule as writes so an edit made after the deleting client fetched the index
/// cannot be erased silently.
pub enum MonitorDeleteOutcome {
    Deleted,
    NotFound,
    VersionMismatch {
        current_version: i64,
        current_updated_at: i64,
    },
}
