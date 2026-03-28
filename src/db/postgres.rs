use anyhow::{Context, Result};
use sqlx::PgPool;

use super::models::*;
use super::DatabaseRepo;

pub struct PostgresRepo {
    pool: PgPool,
}

impl PostgresRepo {
    pub async fn open(url: &str) -> Result<Self> {
        let pool = PgPool::connect(url).await
            .with_context(|| format!("failed to connect to PostgreSQL: {url}"))?;

        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id            TEXT PRIMARY KEY,
                username      TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                role          TEXT NOT NULL DEFAULT 'user',
                created_at    BIGINT NOT NULL,
                updated_at    BIGINT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create users")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS devices (
                id            TEXT PRIMARY KEY,
                user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                name          TEXT NOT NULL,
                device_key    TEXT,
                created_at    BIGINT NOT NULL,
                last_seen_at  BIGINT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create devices")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id)")
            .execute(&self.pool)
            .await
            .context("migrate: idx_devices_user_id")?;

        // Partial unique index on device_key (only non-NULL values)
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_devices_device_key ON devices(device_key) WHERE device_key IS NOT NULL"
        )
        .execute(&self.pool)
        .await
        .context("migrate: idx_devices_device_key")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cursors (
                device_id   TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
                provider    TEXT NOT NULL DEFAULT '',
                last_ts     BIGINT NOT NULL DEFAULT 0,
                updated_at  BIGINT NOT NULL,
                PRIMARY KEY (device_id, provider)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create cursors")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS refresh_tokens (
                jti         TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                device_id   TEXT,
                expires_at  BIGINT NOT NULL,
                revoked     SMALLINT NOT NULL DEFAULT 0,
                created_at  BIGINT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create refresh_tokens")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_refresh_tokens_user ON refresh_tokens(user_id)")
            .execute(&self.pool)
            .await
            .context("migrate: idx_refresh_tokens_user")?;

        // Migration: teams and team_members tables
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS teams (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                created_at  BIGINT NOT NULL,
                updated_at  BIGINT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create teams")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS team_members (
                team_id   TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
                user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role      TEXT NOT NULL DEFAULT 'member',
                joined_at BIGINT NOT NULL,
                PRIMARY KEY (team_id, user_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migrate: create team_members")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_team_members_user ON team_members(user_id)")
            .execute(&self.pool)
            .await
            .context("migrate: idx_team_members_user")?;

        // Migration: add OIDC columns to users
        // Use DO $$ block to avoid errors if columns already exist
        sqlx::query("ALTER TABLE users ADD COLUMN IF NOT EXISTS oidc_sub TEXT")
            .execute(&self.pool).await.context("migrate: add oidc_sub")?;
        sqlx::query("ALTER TABLE users ADD COLUMN IF NOT EXISTS oidc_issuer TEXT")
            .execute(&self.pool).await.context("migrate: add oidc_issuer")?;
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_oidc ON users(oidc_issuer, oidc_sub) WHERE oidc_sub IS NOT NULL"
        )
        .execute(&self.pool).await.context("migrate: idx_users_oidc")?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl DatabaseRepo for PostgresRepo {
    // ── Users ───────────────────────────────────────────────────────────────

    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let row: Option<(String, String, String, String, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer FROM users WHERE username = $1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .context("get_user_by_username")?;

        Ok(row.map(|(id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer)| User {
            id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer,
        }))
    }

    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        let row: Option<(String, String, String, String, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("get_user_by_id")?;

        Ok(row.map(|(id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer)| User {
            id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer,
        }))
    }

    async fn create_user(&self, user: &NewUser) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(&user.id)
        .bind(&user.username)
        .bind(&user.password_hash)
        .bind(&user.role)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("create_user")?;
        Ok(())
    }

    async fn delete_user(&self, id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete_user")?
            .rows_affected();
        Ok(affected > 0)
    }

    async fn update_password(&self, user_id: &str, password_hash: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE users SET password_hash = $1, updated_at = $2 WHERE id = $3")
            .bind(password_hash)
            .bind(now)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("update_password")?;
        Ok(())
    }

    async fn list_users(&self) -> Result<Vec<UserSummary>> {
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT id, username, role, created_at FROM users ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .context("list_users")?;

        Ok(rows.into_iter().map(|(id, username, role, created_at)| UserSummary {
            id, username, role, created_at,
        }).collect())
    }

    async fn user_is_admin(&self, user_id: &str) -> Result<bool> {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .context("user_is_admin")?;
        Ok(role.as_deref() == Some("admin"))
    }

    // ── Devices ─────────────────────────────────────────────────────────────

    async fn find_device_by_key_and_user(&self, device_key: &str, user_id: &str) -> Result<Option<String>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM devices WHERE device_key = $1 AND user_id = $2",
        )
        .bind(device_key)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("find_device_by_key_and_user")?;
        Ok(id)
    }

    async fn create_device(&self, id: &str, user_id: &str, name: &str, device_key: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO devices (id, user_id, name, device_key, created_at, last_seen_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(user_id)
        .bind(name)
        .bind(device_key)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("create_device")?;
        Ok(())
    }

    async fn update_device_seen(&self, id: &str, name: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE devices SET last_seen_at = $1, name = $2 WHERE id = $3")
            .bind(now)
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("update_device_seen")?;
        Ok(())
    }

    async fn list_user_devices(&self, user_id: &str) -> Result<Vec<DeviceSummary>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT id, name, last_seen_at FROM devices WHERE user_id = $1 ORDER BY last_seen_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("list_user_devices")?;

        Ok(rows.into_iter().map(|(id, name, last_seen_at)| DeviceSummary {
            id, name, last_seen_at,
        }).collect())
    }

    async fn list_all_devices(&self) -> Result<Vec<DeviceAdminSummary>> {
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT d.id, d.name, u.username, d.last_seen_at FROM devices d
             JOIN users u ON d.user_id = u.id ORDER BY d.last_seen_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("list_all_devices")?;

        Ok(rows.into_iter().map(|(id, name, username, last_seen_at)| DeviceAdminSummary {
            id, name, username, last_seen_at,
        }).collect())
    }

    async fn delete_device(&self, id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM devices WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete_device")?
            .rows_affected();
        Ok(affected > 0)
    }

    async fn delete_user_device(&self, id: &str, user_id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM devices WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("delete_user_device")?
            .rows_affected();
        Ok(affected > 0)
    }

    async fn device_belongs_to_user(&self, device_id: &str, user_id: &str) -> Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM devices WHERE id = $1 AND user_id = $2"
        )
        .bind(device_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .context("device_belongs_to_user")?;
        Ok(exists)
    }

    async fn rename_device(&self, device_id: &str, user_id: &str, name: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE devices SET name = $1 WHERE id = $2 AND user_id = $3",
        )
        .bind(name)
        .bind(device_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .context("rename_device")?
        .rows_affected();
        Ok(affected > 0)
    }

    async fn get_user_device_ids(&self, user_id: &str) -> Result<Vec<String>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM devices WHERE user_id = $1"
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("get_user_device_ids")?;
        Ok(ids)
    }

    // ── Cursors ─────────────────────────────────────────────────────────────

    async fn ensure_cursor(&self, device_id: &str, provider: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO cursors (device_id, provider, last_ts, updated_at) VALUES ($1, $2, 0, $3) ON CONFLICT DO NOTHING",
        )
        .bind(device_id)
        .bind(provider)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("ensure_cursor")?;
        Ok(())
    }

    async fn get_last_ts(&self, device_id: &str, provider: &str) -> Result<i64> {
        let ts: Option<i64> = sqlx::query_scalar(
            "SELECT last_ts FROM cursors WHERE device_id = $1 AND provider = $2",
        )
        .bind(device_id)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await
        .context("get_last_ts")?;
        Ok(ts.unwrap_or(0))
    }

    async fn advance_cursor(&self, device_id: &str, provider: &str, ts: i64) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "UPDATE cursors SET last_ts = GREATEST(last_ts, $1), updated_at = $2 WHERE device_id = $3 AND provider = $4",
        )
        .bind(ts)
        .bind(now)
        .bind(device_id)
        .bind(provider)
        .execute(&self.pool)
        .await
        .context("advance_cursor")?;
        Ok(())
    }

    // ── Teams ───────────────────────────────────────────────────────────────

    async fn create_team(&self, id: &str, name: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO teams (id, name, created_at, updated_at) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await
            .context("create_team")?;
        Ok(())
    }

    async fn get_team(&self, id: &str) -> Result<Option<Team>> {
        let row: Option<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT id, name, created_at, updated_at FROM teams WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("get_team")?;

        Ok(row.map(|(id, name, created_at, updated_at)| Team {
            id, name, created_at, updated_at,
        }))
    }

    async fn list_teams(&self) -> Result<Vec<Team>> {
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT id, name, created_at, updated_at FROM teams ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .context("list_teams")?;

        Ok(rows.into_iter().map(|(id, name, created_at, updated_at)| Team {
            id, name, created_at, updated_at,
        }).collect())
    }

    async fn delete_team(&self, id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM teams WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete_team")?
            .rows_affected();
        Ok(affected > 0)
    }

    async fn list_user_teams(&self, user_id: &str) -> Result<Vec<TeamMembership>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT tm.team_id, t.name, tm.role FROM team_members tm
             JOIN teams t ON tm.team_id = t.id
             WHERE tm.user_id = $1 ORDER BY t.name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("list_user_teams")?;

        Ok(rows.into_iter().map(|(team_id, team_name, role)| TeamMembership {
            team_id, team_name, role,
        }).collect())
    }

    // ── Team members ───────────────────────────────────────────────────────

    async fn add_team_member(&self, team_id: &str, user_id: &str, role: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO team_members (team_id, user_id, role, joined_at) VALUES ($1, $2, $3, $4)"
        )
        .bind(team_id)
        .bind(user_id)
        .bind(role)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("add_team_member")?;
        Ok(())
    }

    async fn remove_team_member(&self, team_id: &str, user_id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
            .bind(team_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("remove_team_member")?
            .rows_affected();
        Ok(affected > 0)
    }

    async fn list_team_members(&self, team_id: &str) -> Result<Vec<TeamMemberSummary>> {
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT tm.user_id, u.username, tm.role, tm.joined_at FROM team_members tm
             JOIN users u ON tm.user_id = u.id
             WHERE tm.team_id = $1 ORDER BY tm.joined_at",
        )
        .bind(team_id)
        .fetch_all(&self.pool)
        .await
        .context("list_team_members")?;

        Ok(rows.into_iter().map(|(user_id, username, role, joined_at)| TeamMemberSummary {
            user_id, username, role, joined_at,
        }).collect())
    }

    async fn get_team_member_role(&self, team_id: &str, user_id: &str) -> Result<Option<String>> {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM team_members WHERE team_id = $1 AND user_id = $2"
        )
        .bind(team_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("get_team_member_role")?;
        Ok(role)
    }

    // ── OIDC users ─────────────────────────────────────────────────────────

    async fn find_user_by_oidc(&self, issuer: &str, sub: &str) -> Result<Option<User>> {
        let row: Option<(String, String, String, String, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer FROM users WHERE oidc_issuer = $1 AND oidc_sub = $2",
        )
        .bind(issuer)
        .bind(sub)
        .fetch_optional(&self.pool)
        .await
        .context("find_user_by_oidc")?;

        Ok(row.map(|(id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer)| User {
            id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer,
        }))
    }

    async fn create_oidc_user(&self, user: &NewOidcUser) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at, updated_at, oidc_sub, oidc_issuer) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&user.id)
        .bind(&user.username)
        .bind("")  // no password for OIDC users
        .bind(&user.role)
        .bind(now)
        .bind(now)
        .bind(&user.oidc_sub)
        .bind(&user.oidc_issuer)
        .execute(&self.pool)
        .await
        .context("create_oidc_user")?;
        Ok(())
    }

    // ── Refresh tokens ──────────────────────────────────────────────────────

    async fn store_refresh_token(&self, jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO refresh_tokens (jti, user_id, device_id, expires_at, revoked, created_at) VALUES ($1, $2, $3, $4, 0, $5)",
        )
        .bind(jti)
        .bind(user_id)
        .bind(device_id)
        .bind(expires_at)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("store_refresh_token")?;
        Ok(())
    }

    async fn is_refresh_token_revoked(&self, jti: &str) -> Result<bool> {
        let revoked: Option<bool> = sqlx::query_scalar(
            "SELECT revoked != 0 FROM refresh_tokens WHERE jti = $1",
        )
        .bind(jti)
        .fetch_optional(&self.pool)
        .await
        .context("is_refresh_token_revoked")?;
        Ok(revoked.unwrap_or(true))
    }

    async fn revoke_refresh_token(&self, jti: &str) -> Result<()> {
        sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE jti = $1")
            .bind(jti)
            .execute(&self.pool)
            .await
            .context("revoke_refresh_token")?;
        Ok(())
    }

    async fn revoke_user_refresh_tokens(&self, user_id: &str) -> Result<()> {
        sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE user_id = $1 AND revoked = 0")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("revoke_user_refresh_tokens")?;
        Ok(())
    }

    async fn rotate_refresh_token(&self, old_jti: &str, new_jti: &str, user_id: &str, device_id: Option<&str>, expires_at: i64) -> Result<()> {
        let mut tx = self.pool.begin().await.context("failed to begin transaction")?;

        // Check not already revoked
        let revoked: Option<bool> = sqlx::query_scalar(
            "SELECT revoked != 0 FROM refresh_tokens WHERE jti = $1",
        )
        .bind(old_jti)
        .fetch_optional(&mut *tx)
        .await
        .context("db error checking revocation")?;

        let is_revoked = revoked.unwrap_or(true);

        if is_revoked {
            tx.rollback().await.ok();
            return Err(anyhow::anyhow!("refresh token already used or revoked"));
        }

        // Revoke old token
        sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE jti = $1")
            .bind(old_jti)
            .execute(&mut *tx)
            .await
            .context("failed to revoke old refresh token")?;

        // Store new refresh token
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO refresh_tokens (jti, user_id, device_id, expires_at, revoked, created_at) VALUES ($1, $2, $3, $4, 0, $5)",
        )
        .bind(new_jti)
        .bind(user_id)
        .bind(device_id)
        .bind(expires_at)
        .bind(now)
        .execute(&mut *tx)
        .await
        .context("failed to store new refresh token")?;

        tx.commit().await.context("failed to commit rotation transaction")?;
        Ok(())
    }
}
