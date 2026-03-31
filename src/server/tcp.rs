use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{watch, Semaphore};

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::events::EventStore;
use crate::sync::handler::handle_connection;

const MAX_TCP_CONNECTIONS: usize = 500;

/// Run the TCP sync server.
///
/// Listens on `addr`, accepts connections, and spawns a handler task per client.
/// Shuts down cleanly when `shutdown_rx` receives `true`.
pub async fn run_tcp_server(
    db:  Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    events: Arc<dyn EventStore>,
    addr: SocketAddr,
    max_concurrent_writes: usize,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let semaphore = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    let batch_semaphore = Arc::new(Semaphore::new(max_concurrent_writes));
    tracing::info!("TCP sync server listening on {addr} (max_concurrent_writes={max_concurrent_writes})");

    loop {
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

                let handle = tokio::spawn(async move {
                    tracing::debug!("TCP connection from {peer_addr}");
                    if let Err(e) = handle_connection(stream, db, jwt, ev, batch_sem).await {
                        tracing::warn!("TCP connection error from {peer_addr}: {e}");
                    }
                    drop(permit); // released when connection closes
                });

                // Monitor task: detect panics in the handler
                tokio::spawn(async move {
                    if let Err(e) = handle.await {
                        tracing::error!("TCP handler task panicked for {peer_addr}: {e}");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("TCP sync server stopping");
                    break;
                }
            }
        }
    }

    Ok(())
}
