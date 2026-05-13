use tower_http::cors::{Any, CorsLayer};

mod config;
mod error;
mod middleware;
mod models;
mod queue;
mod repository;
mod routes;
mod services;
mod state;

use config::AppConfig;
use routes::init_routes;
use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls ring crypto provider");

    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env().map_err(|e| anyhow::anyhow!(e))?;
    let state = AppState::new(config.clone())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tracing::info!(
        runs_repository_backend = %state.runs_repo.backend_name(),
        "runs repository initialized"
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = init_routes().with_state(state).layer(cors);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("altair-ia-ms running on http://{}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,axum=info,tower_http=info".into());

    tracing_subscriber::fmt().with_env_filter(filter).init();
}
