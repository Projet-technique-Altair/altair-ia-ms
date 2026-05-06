use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post, put},
    Router,
};

use crate::state::AppState;

const LOCAL_STORAGE_MAX_BODY_BYTES: usize = 30 * 1024 * 1024;

pub mod execute;
pub mod health;
pub mod internal;
pub mod local_storage;
pub mod runs;
pub mod uploads;

pub fn init_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route(
            "/api/ia/labs/uploads/presign",
            post(uploads::presign_uploads),
        )
        .route(
            "/api/ia/labs/execute/structured",
            post(execute::execute_structured_run),
        )
        .route("/api/ia/labs/runs/{id}", get(runs::get_run_status))
        .route(
            "/api/ia/labs/runs/{id}/download/presign",
            post(runs::presign_download),
        )
        .route(
            "/internal/ia/runs/{id}/process",
            post(internal::process_run_internal),
        )
        .route(
            "/internal/ia/pedagogical-analysis",
            post(internal::pedagogical_analysis_internal),
        )
        .route(
            "/api/ia/local-storage/{*object_key}",
            put(local_storage::put_local_object)
                .get(local_storage::get_local_object)
                .layer(DefaultBodyLimit::max(LOCAL_STORAGE_MAX_BODY_BYTES)),
        )
        .route(
            "/local-storage/{*object_key}",
            put(local_storage::put_local_object)
                .get(local_storage::get_local_object)
                .layer(DefaultBodyLimit::max(LOCAL_STORAGE_MAX_BODY_BYTES)),
        )
}
