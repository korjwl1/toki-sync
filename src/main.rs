mod auth;
mod config;
mod db;
mod metrics;
mod server;
mod sync;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::{BruteForceGuard, JwtManager};
use crate::config::Config;
use crate::db::{DatabaseRepo, open_database};
use crate::metrics::VictoriaMetrics;
use crate::server::{build_router, http::AppState, tcp::run_tcp_server};

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first (needed for log config)
    let config_path = std::env::var("TOKI_SYNC_CONFIG")
        .unwrap_or_else(|_| "./config.toml".to_string());
    let config = Config::load_or_default(std::path::Path::new(&config_path))
        .context("failed to load config")?;

    // Initialize tracing with config-driven level + format
    init_tracing(&config.log.level, config.log.json);

    tracing::info!("toki-sync starting (http=:{}, tcp=:{})",
        config.server.http_port, config.server.tcp_port);

    if config.auth.jwt_secret == "change-me-in-production" {
        tracing::warn!("Using default JWT secret -- set JWT_SECRET env var for production!");
    }

    let mode = config.auth.effective_registration_mode();
    if !["open", "approval", "closed"].contains(&mode) {
        tracing::error!("Invalid registration_mode: '{}'. Must be 'open', 'approval', or 'closed'.", mode);
        std::process::exit(1);
    }

    // Open database
    let db = open_database(&config.storage)
        .await
        .context("failed to open database")?;

    // Ensure admin account exists (TOKI_ADMIN_PASSWORD env)
    ensure_admin(&*db).await?;

    // Cleanup expired/revoked refresh tokens
    let cleaned = db.cleanup_expired_tokens().await
        .context("failed to cleanup expired tokens")?;
    if cleaned > 0 {
        tracing::info!("cleaned up {cleaned} expired/revoked refresh tokens");
    }

    // Cleanup old pending registrations (older than 7 days)
    let cleaned_pending = db.cleanup_old_pending_registrations(7 * 86400).await
        .context("failed to cleanup old pending registrations")?;
    if cleaned_pending > 0 {
        tracing::info!("cleaned up {cleaned_pending} old pending registrations");
    }

    // Cleanup expired device codes
    let cleaned_dc = db.cleanup_expired_device_codes().await
        .context("failed to cleanup expired device codes")?;
    if cleaned_dc > 0 {
        tracing::info!("cleaned up {cleaned_dc} expired device codes");
    }

    // Build shared state
    let jwt_manager = JwtManager::new(
        &config.auth.jwt_secret,
        config.auth.access_token_ttl_secs,
        config.auth.refresh_token_ttl_secs,
    );
    let jwt = Arc::new(if !config.server.external_url.is_empty() {
        jwt_manager.with_issuer(&config.server.external_url)
    } else {
        jwt_manager
    });
    let brute = Arc::new(BruteForceGuard::new(
        config.auth.brute_force_max_attempts,
        config.auth.brute_force_window_secs,
        config.auth.brute_force_lockout_secs,
    ));
    let vm = Arc::new(VictoriaMetrics::new(&config.backend.vm_url));
    let oidc_state_store = Arc::new(crate::auth::oidc::OidcStateStore::new(600)); // 10 min TTL

    if !config.auth.oidc_issuer.is_empty() {
        tracing::info!(issuer = %config.auth.oidc_issuer, "OIDC authentication enabled");
    }

    // Derive OIDC redirect_uri from external_url if not explicitly set
    let oidc_redirect_uri = if config.auth.oidc_redirect_uri.is_empty() {
        if !config.server.external_url.is_empty() {
            format!("{}/auth/callback", config.server.external_url)
        } else {
            String::new()
        }
    } else {
        config.auth.oidc_redirect_uri.clone()
    };

    let state = AppState {
        db, jwt, brute, vm,
        registration_mode: config.auth.effective_registration_mode().to_string(),
        access_token_ttl_secs: config.auth.access_token_ttl_secs,
        oidc_issuer: config.auth.oidc_issuer.clone(),
        oidc_client_id: config.auth.oidc_client_id.clone(),
        oidc_client_secret: config.auth.oidc_client_secret.clone(),
        oidc_redirect_uri,
        oidc_state_store,
        oidc_discovery_cache: Arc::new(tokio::sync::RwLock::new(None)),
        oidc_http_client: reqwest::Client::new(),
        external_url: config.server.external_url.clone(),
        storage_backend: config.storage.backend.clone(),
        device_poll_tracker: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };

    // -- TCP sync server ------------------------------------------------------
    let tcp_addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.tcp_port)
        .parse()
        .context("invalid TCP bind address")?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let tcp_db  = state.db.clone();
    let tcp_jwt = state.jwt.clone();
    let tcp_vm  = state.vm.clone();
    let max_concurrent_writes = config.server.max_concurrent_writes;

    tokio::spawn(async move {
        if let Err(e) = run_tcp_server(tcp_db, tcp_jwt, tcp_vm, tcp_addr, max_concurrent_writes, shutdown_rx).await {
            tracing::error!("TCP server error: {e}");
        }
    });

    // -- HTTP server ----------------------------------------------------------
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

async fn ensure_admin(db: &dyn DatabaseRepo) -> Result<()> {
    let admin_password = match std::env::var("TOKI_ADMIN_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("TOKI_ADMIN_PASSWORD not set, skipping admin auto-creation");
            return Ok(());
        }
    };

    let existing = db.get_user_by_username("admin").await
        .context("failed to check admin existence")?;

    if existing.is_some() {
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

    let new_user = db::models::NewUser {
        id: uuid::Uuid::new_v4().to_string(),
        username: "admin".to_string(),
        password_hash: hash,
        role: "admin".to_string(),
    };
    db.create_user(&new_user).await
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
