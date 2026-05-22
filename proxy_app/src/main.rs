use std::{net::SocketAddr, process, time::Duration};
use tokio::time::timeout;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "proxy_app=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = proxy_config::load_from_env().unwrap_or_else(|err| {
        tracing::error!("failed to load config: {err}");
        process::exit(1);
    });
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .unwrap_or_else(|err| {
            tracing::error!("failed to parse socket address: {err}");
            process::exit(1);
        });
    let graceful_shutdown_timeout_secs = config.graceful_shutdown_timeout_secs;
    let state = proxy_app::state::AppState::from_config(config);
    let app = proxy_app::build_app_with_state(state);
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    match timeout(Duration::from_secs(graceful_shutdown_timeout_secs), server).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::error!("server error: {err}"),
        Err(_) => tracing::warn!("graceful shutdown timed out, forcing exit"),
    }
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!("failed to listen for shutdown signal: {err}");
    }
}
