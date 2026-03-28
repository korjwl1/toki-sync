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
                device_id   TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
                last_ts     INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL
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

        // Migration: add provider column to cursors for multi-provider support.
        // ALTER TABLE fails silently on subsequent runs (column already exists).
        let _ = sqlx::query(
            "ALTER TABLE cursors ADD COLUMN provider TEXT NOT NULL DEFAULT ''",
        )
        .execute(&self.pool)
        .await;

        // Unique index enables (device_id, provider) cursor per provider per device.
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_cursors_device_provider \
             ON cursors(device_id, provider)",
        )
        .execute(&self.pool)
        .await
        .context("failed to create cursor index")?;

        Ok(())
    }
}
