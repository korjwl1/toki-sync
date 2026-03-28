mod auth;
mod config;
mod db;
mod metrics;
mod server;
mod sync;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use crate::auth::{BruteForceGuard, JwtManager};
use crate::config::Config;
use crate::db::Database;
use crate::metrics::VictoriaMetrics;
use crate::server::{build_router, http::AppState, tcp::run_tcp_server};

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first (needed for log config)
    let config_path = std::env::var("TOKI_SYNC_CONFIG")
        .unwrap_or_else(|_| "./config.toml".to_string());
    let config = Config::load_or_default(Path::new(&config_path))
        .context("failed to load config")?;

    // Initialize tracing with config-driven level + format
    init_tracing(&config.log.level, config.log.json);

    tracing::info!("toki-sync starting (http=:{}, tcp=:{})",
        config.server.http_port, config.server.tcp_port);

    // Open database
    let db = Arc::new(
        Database::open(&config.storage.db_path)
            .await
            .context("failed to open database")?,
    );

    // Ensure admin account exists (TOKI_ADMIN_PASSWORD env)
    ensure_admin(&db).await?;

    // Build shared state
    let jwt = Arc::new(JwtManager::new(
        &config.auth.jwt_secret,
        config.auth.access_token_ttl_secs,
        config.auth.refresh_token_ttl_secs,
    ));
    let brute = Arc::new(BruteForceGuard::new(
        config.auth.brute_force_max_attempts,
        config.auth.brute_force_window_secs,
        config.auth.brute_force_lockout_secs,
    ));
    let vm = Arc::new(VictoriaMetrics::new(&config.backend.vm_url));

    let state = AppState { db, jwt, brute, vm };

    // ── TCP sync server ─────────────────────────────────────────────────────
    let tcp_addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.tcp_port)
        .parse()
        .context("invalid TCP bind address")?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let tcp_db  = state.db.clone();
    let tcp_jwt = state.jwt.clone();
    let tcp_vm  = state.vm.clone();

    tokio::spawn(async move {
        if let Err(e) = run_tcp_server(tcp_db, tcp_jwt, tcp_vm, tcp_addr, shutdown_rx).await {
            tracing::error!("TCP server error: {e}");
        }
    });

    // ── HTTP server ──────────────────────────────────────────────────────────
    let router = build_router(state).into_make_service_with_connect_info::<SocketAddr>();

    let http_addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.http_port)
        .parse()
        .context("invalid HTTP bind address")?;

    tracing::info!("HTTP server listening on {http_addr}");

    let listener = tokio::net::TcpListener::bind(http_addr).await
        .with_context(|| format!("failed to bind to {http_addr}"))?;

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx.send(true);
        })
        .await
        .context("HTTP server error")?;

    tracing::info!("toki-sync stopped");
    Ok(())
}

fn init_tracing(level: &str, json: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    if json {
        fmt().json()
            .with_env_filter(filter)
            .with_current_span(false)
            .init();
    } else {
        fmt()
            .with_env_filter(filter)
            .init();
    }
}

async fn ensure_admin(db: &Database) -> Result<()> {
    let admin_password = match std::env::var("TOKI_ADMIN_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("TOKI_ADMIN_PASSWORD not set, skipping admin auto-creation");
            return Ok(());
        }
    };

    let exists: bool = sqlx::query_scalar("SELECT COUNT(*) > 0 FROM users WHERE username = 'admin'")
        .fetch_one(&db.pool)
        .await
        .context("failed to check admin existence")?;

    if exists {
        tracing::debug!("admin account already exists, skipping");
        return Ok(());
    }

    // Hash password in threadpool (bcrypt is CPU-intensive)
    let hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(&admin_password, bcrypt::DEFAULT_COST)
    })
    .await
    .context("bcrypt task panicked")?
    .context("bcrypt hashing failed")?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at)
         VALUES (?, 'admin', ?, 'admin', ?, ?)",
    )
    .bind(&id)
    .bind(&hash)
    .bind(now)
    .bind(now)
    .execute(&db.pool)
    .await
    .context("failed to create admin user")?;

    tracing::info!("admin account created");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
