/// Shared model types used by all database backends.

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
