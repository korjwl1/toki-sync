use anyhow::{Context, Result};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn open(db_path: &str) -> Result<Self> {
        // Create parent directories if needed
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create db dir: {}", parent.display()))?;
            }
        }

        let opts = SqliteConnectOptions::from_str(db_path)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePool::connect_with(opts).await
            .with_context(|| format!("failed to open SQLite: {db_path}"))?;

        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id          TEXT PRIMARY KEY,
                username    TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                role        TEXT NOT NULL DEFAULT 'user',
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS devices (
                id          TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                name        TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id);

            CREATE TABLE IF NOT EXISTS cursors (
                device_id   TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
                provider    TEXT NOT NULL DEFAULT '',
                last_ts     INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (device_id, provider)
            );

            CREATE TABLE IF NOT EXISTS refresh_tokens (
                jti         TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                device_id   TEXT,
                expires_at  INTEGER NOT NULL,
                revoked     INTEGER NOT NULL DEFAULT 0,
                created_at  INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_refresh_tokens_user ON refresh_tokens(user_id);
            "#,
        )
        .execute(&self.pool)
        .await
        .context("migration failed")?;

        // Migration: recreate cursors table with composite PK (device_id, provider)
        // Only needed if the old single-PK schema exists.
        let has_old_schema: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('cursors') WHERE name = 'device_id' AND pk = 1"
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false);

        if has_old_schema {
            // Check if provider column already exists (from previous migration)
            let has_provider: bool = sqlx::query_scalar(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('cursors') WHERE name = 'provider'"
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(false);

            sqlx::query("ALTER TABLE cursors RENAME TO cursors_old").execute(&self.pool).await.ok();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS cursors (
                    device_id   TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
                    provider    TEXT NOT NULL DEFAULT '',
                    last_ts     INTEGER NOT NULL DEFAULT 0,
                    updated_at  INTEGER NOT NULL,
                    PRIMARY KEY (device_id, provider)
                )"
            ).execute(&self.pool).await?;

            if has_provider {
                sqlx::query("INSERT OR IGNORE INTO cursors SELECT device_id, provider, last_ts, updated_at FROM cursors_old")
                    .execute(&self.pool).await.ok();
            } else {
                sqlx::query("INSERT OR IGNORE INTO cursors SELECT device_id, '', last_ts, updated_at FROM cursors_old")
                    .execute(&self.pool).await.ok();
            }

            sqlx::query("DROP TABLE IF EXISTS cursors_old").execute(&self.pool).await.ok();
        }

        // Migration: add device_key column to devices for stable UUID-based identity.
        // ALTER TABLE fails silently on subsequent runs (column already exists).
        let _ = sqlx::query(
            "ALTER TABLE devices ADD COLUMN device_key TEXT",
        )
        .execute(&self.pool)
        .await;

        Ok(())
    }
}
