use axum::http::{HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

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

const DEFAULT_ALLOWED_ORIGINS: &str = "http://localhost:5173,http://localhost:3000";
const DEFAULT_ALLOWED_METHODS: &str = "GET,POST,PUT,OPTIONS";
const DEFAULT_ALLOWED_HEADERS: &str =
    "authorization,content-type,x-altair-user-id,x-altair-roles,x-altair-role,x-user-id,x-user-roles,x-user-role,x-internal-worker-token";

fn parse_allowed_origins() -> Vec<HeaderValue> {
    std::env::var("ALLOWED_ORIGINS")
        .unwrap_or_else(|_| DEFAULT_ALLOWED_ORIGINS.to_string())
        .split(',')
        .filter_map(|origin| HeaderValue::from_str(origin.trim()).ok())
        .collect()
}

fn parse_allowed_methods() -> Vec<Method> {
    std::env::var("ALLOWED_METHODS")
        .unwrap_or_else(|_| DEFAULT_ALLOWED_METHODS.to_string())
        .split(',')
        .filter_map(|method| Method::from_bytes(method.trim().as_bytes()).ok())
        .collect()
}

fn parse_allowed_headers() -> Vec<HeaderName> {
    std::env::var("ALLOWED_HEADERS")
        .unwrap_or_else(|_| DEFAULT_ALLOWED_HEADERS.to_string())
        .split(',')
        .filter_map(|header| {
            HeaderName::from_bytes(header.trim().to_ascii_lowercase().as_bytes()).ok()
        })
        .collect()
}

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
        .allow_origin(AllowOrigin::list(parse_allowed_origins()))
        .allow_methods(parse_allowed_methods())
        .allow_headers(parse_allowed_headers());

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
