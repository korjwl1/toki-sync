use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::watch;

use crate::auth::JwtManager;
use crate::db::Database;
use crate::metrics::VictoriaMetrics;
use crate::sync::handler::handle_connection;

/// Run the TCP sync server.
///
/// Listens on `addr`, accepts connections, and spawns a handler task per client.
/// Shuts down cleanly when `shutdown_rx` receives `true`.
pub async fn run_tcp_server(
    db:  Arc<Database>,
    jwt: Arc<JwtManager>,
    vm:  Arc<VictoriaMetrics>,
    addr: SocketAddr,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("TCP sync server listening on {addr}");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = result?;
                let db  = db.clone();
                let jwt = jwt.clone();
                let vm  = vm.clone();

                tokio::spawn(async move {
                    tracing::debug!("TCP connection from {peer_addr}");
                    if let Err(e) = handle_connection(stream, db, jwt, vm).await {
                        tracing::warn!("TCP connection error from {peer_addr}: {e}");
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
