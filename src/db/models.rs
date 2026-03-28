/// Shared model types used by all database backends.

#[allow(dead_code)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewUser {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
}

pub struct UserSummary {
    pub id: String,
    pub username: String,
    pub role: String,
    pub created_at: i64,
}

pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub last_seen_at: i64,
}

pub struct DeviceAdminSummary {
    pub id: String,
    pub name: String,
    pub username: String,
    pub last_seen_at: i64,
}
