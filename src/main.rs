mod gpx_parse;
mod handlers;
mod hours;
mod overpass;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;

use crate::handlers::AppState;
use crate::overpass::OverpassCache;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ravito_gpx=info")),
        )
        .init();

    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "data/ravito.db".into());
    if let Some(parent) = PathBuf::from(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let overpass_url = std::env::var("OVERPASS_URL")
        .unwrap_or_else(|_| "https://overpass-api.de/api/interpreter".into());
    let cache = Arc::new(OverpassCache::open(&db_path, overpass_url)?);

    let state = AppState { cache };

    let app = Router::new()
        .route("/", get(handlers::index))
        .route("/static/app.css", get(handlers::app_css))
        .route("/api/analyze", post(handlers::analyze))
        .with_state(state)
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(TraceLayer::new_for_http());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "ravito_gpx listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sig) = signal(SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown");
}
