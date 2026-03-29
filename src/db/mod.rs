pub mod models;
pub mod sqlite;
pub mod postgres;

use std::sync::Arc;
use anyhow::Result;
use models::*;

use crate::config::StorageConfig;

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait DatabaseRepo: Send + Sync {
    // Users
    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>>;
    async fn create_user(&self, user: &NewUser) -> Result<()>;
    async fn delete_user(&self, id: &str) -> Result<bool>;
    async fn update_password(&self, user_id: &str, password_hash: &str) -> Result<()>;
    async fn list_users(&self) -> Result<Vec<UserSummary>>;
    async fn user_is_admin(&self, user_id: &str) -> Result<bool>;

    // Devices
    async fn find_device_by_key_and_user(&self, device_key: &str, user_id: &str) -> Result<Option<String>>;
    async fn create_device(&self, id: &str, user_id: &str, name: &str, device_key: &str) -> Result<()>;
    async fn update_device_seen(&self, id: &str, name: &str) -> Result<()>;
    async fn list_user_devices(&self, user_id: &str) -> Result<Vec<DeviceSummary>>;
    async fn list_all_devices(&self) -> Result<Vec<DeviceAdminSummary>>;
    async fn delete_device(&self, id: &str) -> Result<bool>;
    async fn delete_user_device(&self, id: &str, user_id: &str) -> Result<bool>;
    async fn device_belongs_to_user(&self, device_id: &str, user_id: &str) -> Result<bool>;
    async fn rename_device(&self, device_id: &str, user_id: &str, name: &str) -> Result<bool>;
    async fn get_user_device_ids(&self, user_id: &str) -> Result<Vec<String>>;

    // Cursors
    async fn ensure_cursor(&self, device_id: &str, provider: &str) -> Result<()>;
    async fn get_last_ts(&self, device_id: &str, provider: &str) -> Result<i64>;
    async fn advance_cursor(&self, device_id: &str, provider: &str, ts: i64) -> Result<()>;

    // Teams
    async fn create_team(&self, id: &str, name: &str) -> Result<()>;
    async fn get_team(&self, id: &str) -> Result<Option<Team>>;
    async fn list_teams(&self) -> Result<Vec<Team>>;
    async fn list_teams_with_member_count(&self) -> Result<Vec<TeamWithCount>>;
    async fn delete_team(&self, id: &str) -> Result<bool>;
    async fn list_user_teams(&self, user_id: &str) -> Result<Vec<TeamMembership>>;

    // Team members
    async fn add_team_member(&self, team_id: &str, user_id: &str, role: &str) -> Result<()>;
    async fn remove_team_member(&self, team_id: &str, user_id: &str) -> Result<bool>;
    async fn list_team_members(&self, team_id: &str) -> Result<Vec<TeamMemberSummary>>;
    async fn get_team_member_role(&self, team_id: &str, user_id: &str) -> Result<Option<String>>;

    // Pending registrations
    async fn create_pending_registration(&self, id: &str, username: &str, password_hash: &str) -> Result<()>;
    async fn list_pending_registrations(&self) -> Result<Vec<PendingRegistration>>;
    async fn approve_registration(&self, id: &str) -> Result<bool>;
    async fn reject_registration(&self, id: &str) -> Result<bool>;
    async fn cleanup_old_pending_registrations(&self, max_age_secs: i64) -> Result<u64>;

    // User role
    async fn update_user_role(&self, user_id: &str, role: &str) -> Result<bool>;

    // OIDC users
    async fn find_user_by_oidc(&self, issuer: &str, sub: &str) -> Result<Option<User>>;
    async fn create_oidc_user(&self, user: &NewOidcUser) -> Result<()>;

    // Refresh tokens
    async fn store_refresh_token(&self, jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()>;
    async fn is_refresh_token_revoked(&self, jti: &str) -> Result<bool>;
    async fn revoke_refresh_token(&self, jti: &str) -> Result<()>;
    async fn revoke_user_refresh_tokens(&self, user_id: &str) -> Result<()>;
    async fn rotate_refresh_token(&self, old_jti: &str, new_jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()>;

    // Device codes (OAuth 2.0 Device Authorization Grant)
    async fn create_device_code(&self, device_code: &str, user_code: &str, expires_at: i64) -> Result<()>;
    async fn get_device_code(&self, device_code: &str) -> Result<Option<DeviceCode>>;
    async fn get_device_code_by_user_code(&self, user_code: &str) -> Result<Option<DeviceCode>>;
    async fn approve_device_code(&self, user_code: &str, user_id: &str, access_token: &str, refresh_token: &str) -> Result<bool>;
    async fn delete_device_code(&self, device_code: &str) -> Result<()>;
    async fn cleanup_expired_device_codes(&self) -> Result<u64>;

    // Cleanup
    async fn cleanup_expired_tokens(&self) -> Result<u64>;
}

pub async fn open_database(config: &StorageConfig) -> Result<Arc<dyn DatabaseRepo>> {
    match config.backend.as_str() {
        "postgres" => {
            if config.postgres_url.is_empty() {
                anyhow::bail!("storage.backend is 'postgres' but postgres_url is not configured");
            }
            let repo = postgres::PostgresRepo::open(&config.postgres_url).await?;
            Ok(Arc::new(repo))
        }
        _ => {
            let repo = sqlite::SqliteRepo::open(config.effective_sqlite_path()).await?;
            Ok(Arc::new(repo))
        }
    }
}
