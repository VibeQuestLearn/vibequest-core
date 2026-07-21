use std::{net::SocketAddr, time::Duration};

use tracing::info;
use vibequest_core::{app_state, build_router, initialize_platform};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vibequest_core=info,tower_http=info".into()),
        )
        .init();

    let state = app_state();
    if let Err(error) = initialize_platform(&state).await {
        tracing::warn!(error = %error, "v3 index initialization failed");
    }
    let app = build_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], vibequest_core::app_port()));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");

    info!("vibequest-core listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = tokio::time::sleep(Duration::from_secs(u64::MAX)) => {},
    }
}
