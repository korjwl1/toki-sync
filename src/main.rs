mod auth;
mod config;
mod db;
mod metrics;
mod server;
mod sync;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "toki_sync=info".into()),
        )
        .init();

    tracing::info!("toki-sync server starting");

    Ok(())
}
