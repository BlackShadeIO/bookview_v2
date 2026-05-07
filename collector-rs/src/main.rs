#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod binance;
mod polymarket;
mod supervisor;
mod worker;

use bookview_collector::config;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bookview_collector=info".into()),
        )
        .with_target(false)
        .init();

    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => { tracing::info!("Received SIGINT"); }
                _ = sigterm.recv() => { tracing::info!("Received SIGTERM"); }
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            tracing::info!("Received SIGINT");
        }

        shutdown_clone.cancel();
    });

    tracing::info!(pid = std::process::id(), data_dir = %config::DATA_DIR.display(), "Supervisor started");

    supervisor::run(shutdown).await?;

    tracing::info!("Supervisor exited");
    Ok(())
}
