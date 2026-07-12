use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::events::EventStore;
use crate::sync::handler::handle_connection;

const MAX_TCP_CONNECTIONS: usize = 500;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the TCP sync server.
///
/// Listens on `addr`, accepts connections, and spawns a handler task per client.
/// Shuts down cleanly when `shutdown_rx` receives `true`: stops accepting new
/// connections, waits up to 30s for in-flight handlers to complete, then exits.
pub async fn run_tcp_server(
    db:  Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    events: Arc<dyn EventStore>,
    addr: SocketAddr,
    max_concurrent_writes: usize,
    dedup_retention_secs: i64,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let semaphore = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    let batch_semaphore = Arc::new(Semaphore::new(max_concurrent_writes));
    let mut handlers = JoinSet::new();
    tracing::info!("TCP sync server listening on {addr} (max_concurrent_writes={max_concurrent_writes})");

    loop {
        // Reap completed handlers before accepting new ones
        while let Some(result) = handlers.try_join_next() {
            if let Err(e) = result {
                tracing::error!("TCP handler task panicked: {e}");
            }
        }

        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = match result {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("TCP accept error: {e}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!("TCP connection limit reached ({MAX_TCP_CONNECTIONS}), rejecting {peer_addr}");
                        drop(stream);
                        continue;
                    }
                };
                let db  = db.clone();
                let jwt = jwt.clone();
                let ev  = events.clone();
                let batch_sem = batch_semaphore.clone();

                handlers.spawn(async move {
                    tracing::debug!("TCP connection from {peer_addr}");
                    if let Err(e) = handle_connection(stream, db, jwt, ev, batch_sem, dedup_retention_secs).await {
                        tracing::warn!("TCP connection error from {peer_addr}: {e}");
                    }
                    drop(permit);
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }

    // Drain: wait for in-flight connections to finish (up to DRAIN_TIMEOUT)
    let active = handlers.len();
    if active > 0 {
        tracing::info!("TCP sync server draining {active} connections (timeout {DRAIN_TIMEOUT:?})");
        let drain = async {
            while handlers.join_next().await.is_some() {}
        };
        if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
            let remaining = handlers.len();
            tracing::warn!("TCP drain timeout, aborting {remaining} remaining connections");
            handlers.shutdown().await;
        }
    }

    tracing::info!("TCP sync server stopped");
    Ok(())
}
