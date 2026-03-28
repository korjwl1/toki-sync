use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{watch, Semaphore};

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::metrics::VictoriaMetrics;
use crate::sync::handler::handle_connection;

const MAX_TCP_CONNECTIONS: usize = 500;

/// Run the TCP sync server.
///
/// Listens on `addr`, accepts connections, and spawns a handler task per client.
/// Shuts down cleanly when `shutdown_rx` receives `true`.
pub async fn run_tcp_server(
    db:  Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    vm:  Arc<VictoriaMetrics>,
    addr: SocketAddr,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let semaphore = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    tracing::info!("TCP sync server listening on {addr}");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = result?;
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
                let vm  = vm.clone();

                tokio::spawn(async move {
                    tracing::debug!("TCP connection from {peer_addr}");
                    if let Err(e) = handle_connection(stream, db, jwt, vm).await {
                        tracing::warn!("TCP connection error from {peer_addr}: {e}");
                    }
                    drop(permit); // released when connection closes
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
