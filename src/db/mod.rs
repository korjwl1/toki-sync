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

    // Refresh tokens
    async fn store_refresh_token(&self, jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()>;
    async fn is_refresh_token_revoked(&self, jti: &str) -> Result<bool>;
    async fn revoke_refresh_token(&self, jti: &str) -> Result<()>;
    async fn revoke_user_refresh_tokens(&self, user_id: &str) -> Result<()>;
    async fn rotate_refresh_token(&self, old_jti: &str, new_jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()>;
}

pub async fn open_database(config: &StorageConfig) -> Result<Arc<dyn DatabaseRepo>> {
    match config.backend.as_str() {
        "postgres" => {
            let repo = postgres::PostgresRepo::open(&config.postgres_url).await?;
            Ok(Arc::new(repo))
        }
        _ => {
            let repo = sqlite::SqliteRepo::open(config.effective_sqlite_path()).await?;
            Ok(Arc::new(repo))
        }
    }
}
