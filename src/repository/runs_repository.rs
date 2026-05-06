use std::{collections::HashMap, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use sqlx::{types::Json, FromRow, PgPool};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::run::{IaRun, RunMode, RunStatus},
};

#[derive(Clone)]
enum RunsStore {
    InMemory(Arc<RwLock<HashMap<Uuid, IaRun>>>),
    Postgres(PgPool),
}

#[derive(Clone)]
pub struct RunsRepository {
    store: RunsStore,
}

#[derive(Debug, Clone, FromRow)]
struct IaRunRow {
    request_id: Uuid,
    creator_id: String,
    prompt: String,
    status: String,
    mode_selected: Option<String>,
    input_refs: Json<Vec<String>>,
    result_object_key: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    used_model_fallback: bool,
    estimated_input_tokens: Option<i64>,
    actual_input_tokens: Option<i64>,
    actual_output_tokens: Option<i64>,
    excluded_source_objects: Json<Vec<String>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<IaRunRow> for IaRun {
    type Error = AppError;

    fn try_from(row: IaRunRow) -> Result<Self, Self::Error> {
        let status = RunStatus::parse(&row.status).ok_or_else(|| {
            AppError::Internal(format!("invalid run status in database: {}", row.status))
        })?;

        Ok(IaRun {
            request_id: row.request_id,
            creator_id: row.creator_id,
            prompt: row.prompt,
            status,
            mode_selected: row.mode_selected,
            input_refs: row.input_refs.0,
            result_object_key: row.result_object_key,
            error_code: row.error_code,
            error_message: row.error_message,
            used_model_fallback: row.used_model_fallback,
            estimated_input_tokens: row.estimated_input_tokens.map(|v| v as u64),
            actual_input_tokens: row.actual_input_tokens.map(|v| v as u64),
            actual_output_tokens: row.actual_output_tokens.map(|v| v as u64),
            excluded_source_objects: row.excluded_source_objects.0,
            created_at: row.created_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
        })
    }
}

impl RunsRepository {
    pub async fn new(database_url: Option<&str>) -> Result<Self, AppError> {
        let url = database_url.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });

        if let Some(database_url) = url {
            let pool = PgPool::connect(database_url)
                .await
                .map_err(|e| AppError::Internal(format!("failed to connect database: {e}")))?;

            run_optional_migrations(&pool).await?;

            return Ok(Self {
                store: RunsStore::Postgres(pool),
            });
        }

        Ok(Self {
            store: RunsStore::InMemory(Arc::new(RwLock::new(HashMap::new()))),
        })
    }

    pub fn backend_name(&self) -> &'static str {
        match self.store {
            RunsStore::InMemory(_) => "in_memory",
            RunsStore::Postgres(_) => "postgres",
        }
    }

    pub async fn create_run(
        &self,
        request_id: Uuid,
        creator_id: &str,
        prompt: String,
        mode_selected: Option<RunMode>,
        input_refs: Vec<String>,
    ) -> Result<(), AppError> {
        match &self.store {
            RunsStore::InMemory(store) => {
                let mode = mode_selected.map(|m| m.as_str().to_string());
                let run = IaRun::new(request_id, creator_id.to_string(), prompt, mode, input_refs);
                store.write().await.insert(request_id, run);
                Ok(())
            }
            RunsStore::Postgres(pool) => {
                sqlx::query(
                    r#"
                    INSERT INTO ia_runs (
                        request_id,
                        creator_id,
                        prompt,
                        status,
                        mode_selected,
                        input_refs,
                        used_model_fallback,
                        created_at,
                        updated_at
                    )
                    VALUES ($1, $2, $3, 'queued', $4, $5, false, NOW(), NOW())
                    "#,
                )
                .bind(request_id)
                .bind(creator_id)
                .bind(prompt)
                .bind(mode_selected.map(|m| m.as_str().to_string()))
                .bind(Json(input_refs))
                .execute(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to create run: {e}")))?;
                Ok(())
            }
        }
    }

    pub async fn get_run(&self, request_id: Uuid) -> Result<Option<IaRun>, AppError> {
        match &self.store {
            RunsStore::InMemory(store) => Ok(store.read().await.get(&request_id).cloned()),
            RunsStore::Postgres(pool) => {
                let row = sqlx::query_as::<_, IaRunRow>(
                    r#"
                    SELECT
                        request_id,
                        creator_id,
                        prompt,
                        status,
                        mode_selected,
                        input_refs,
                        result_object_key,
                        error_code,
                        error_message,
                        used_model_fallback,
                        estimated_input_tokens,
                        actual_input_tokens,
                        actual_output_tokens,
                        excluded_source_objects,
                        created_at,
                        updated_at,
                        finished_at
                    FROM ia_runs
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to fetch run: {e}")))?;

                match row {
                    Some(db_row) => Ok(Some(IaRun::try_from(db_row)?)),
                    None => Ok(None),
                }
            }
        }
    }

    pub async fn mark_running(&self, request_id: Uuid) -> Result<bool, AppError> {
        match &self.store {
            RunsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(run) = lock.get_mut(&request_id) {
                    if run.status == RunStatus::Queued {
                        run.status = RunStatus::Running;
                        run.updated_at = Utc::now();
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            RunsStore::Postgres(pool) => {
                let result = sqlx::query(
                    r#"
                    UPDATE ia_runs
                    SET status = 'running', updated_at = NOW()
                    WHERE request_id = $1 AND status = 'queued'
                    "#,
                )
                .bind(request_id)
                .execute(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to mark run running: {e}")))?;

                Ok(result.rows_affected() > 0)
            }
        }
    }

    pub async fn mark_completed(
        &self,
        request_id: Uuid,
        result_object_key: &str,
        mode_selected: &str,
        used_model_fallback: bool,
    ) -> Result<(), AppError> {
        match &self.store {
            RunsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(run) = lock.get_mut(&request_id) {
                    run.status = RunStatus::Completed;
                    run.mode_selected = Some(mode_selected.to_string());
                    run.result_object_key = Some(result_object_key.to_string());
                    run.error_code = None;
                    run.error_message = None;
                    run.used_model_fallback = used_model_fallback;
                    run.updated_at = Utc::now();
                    run.finished_at = Some(Utc::now());
                }
                Ok(())
            }
            RunsStore::Postgres(pool) => {
                sqlx::query(
                    r#"
                    UPDATE ia_runs
                    SET
                        status = 'completed',
                        mode_selected = $2,
                        result_object_key = $3,
                        error_code = NULL,
                        error_message = NULL,
                        used_model_fallback = $4,
                        updated_at = NOW(),
                        finished_at = NOW()
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .bind(mode_selected)
                .bind(result_object_key)
                .bind(used_model_fallback)
                .execute(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to mark run completed: {e}")))?;
                Ok(())
            }
        }
    }

    pub async fn update_observability(
        &self,
        request_id: Uuid,
        estimated_input_tokens: Option<u64>,
        actual_input_tokens: Option<u64>,
        actual_output_tokens: Option<u64>,
        excluded_source_objects: &[String],
    ) -> Result<(), AppError> {
        match &self.store {
            RunsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(run) = lock.get_mut(&request_id) {
                    run.estimated_input_tokens = estimated_input_tokens;
                    run.actual_input_tokens = actual_input_tokens;
                    run.actual_output_tokens = actual_output_tokens;
                    run.excluded_source_objects = excluded_source_objects.to_vec();
                    run.updated_at = Utc::now();
                }
                Ok(())
            }
            RunsStore::Postgres(pool) => {
                sqlx::query(
                    r#"
                    UPDATE ia_runs
                    SET
                        estimated_input_tokens = $2,
                        actual_input_tokens = $3,
                        actual_output_tokens = $4,
                        excluded_source_objects = $5,
                        updated_at = NOW()
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .bind(estimated_input_tokens.map(|v| v as i64))
                .bind(actual_input_tokens.map(|v| v as i64))
                .bind(actual_output_tokens.map(|v| v as i64))
                .bind(Json(excluded_source_objects.to_vec()))
                .execute(pool)
                .await
                .map_err(|e| {
                    AppError::Internal(format!("failed to update run observability: {e}"))
                })?;
                Ok(())
            }
        }
    }

    pub async fn mark_requeued_model_unavailable(
        &self,
        request_id: Uuid,
        cycle: u8,
        message: &str,
    ) -> Result<(), AppError> {
        let cycle_message = format!("requeue_cycle={cycle}; {}", message);
        match &self.store {
            RunsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(run) = lock.get_mut(&request_id) {
                    run.status = RunStatus::Queued;
                    run.error_code = Some("MODEL_UNAVAILABLE_RETRY".to_string());
                    run.error_message = Some(cycle_message.clone());
                    run.updated_at = Utc::now();
                    run.finished_at = None;
                }
                Ok(())
            }
            RunsStore::Postgres(pool) => {
                sqlx::query(
                    r#"
                    UPDATE ia_runs
                    SET
                        status = 'queued',
                        error_code = 'MODEL_UNAVAILABLE_RETRY',
                        error_message = $2,
                        updated_at = NOW(),
                        finished_at = NULL
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .bind(cycle_message)
                .execute(pool)
                .await
                .map_err(|e| {
                    AppError::Internal(format!("failed to mark run for model requeue: {e}"))
                })?;
                Ok(())
            }
        }
    }

    pub async fn mark_failed(
        &self,
        request_id: Uuid,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), AppError> {
        match &self.store {
            RunsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(run) = lock.get_mut(&request_id) {
                    run.status = RunStatus::Failed;
                    run.error_code = Some(error_code.to_string());
                    run.error_message = Some(error_message.to_string());
                    run.updated_at = Utc::now();
                    run.finished_at = Some(Utc::now());
                }
                Ok(())
            }
            RunsStore::Postgres(pool) => {
                sqlx::query(
                    r#"
                    UPDATE ia_runs
                    SET
                        status = 'failed',
                        error_code = $2,
                        error_message = $3,
                        updated_at = NOW(),
                        finished_at = NOW()
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .bind(error_code)
                .bind(error_message)
                .execute(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to mark run failed: {e}")))?;
                Ok(())
            }
        }
    }
}

async fn run_optional_migrations(pool: &PgPool) -> Result<(), AppError> {
    let migrations_path = Path::new("./migrations");
    if !migrations_path.exists() {
        tracing::info!("database migrations directory not present; skipping startup migrations");
        return Ok(());
    }

    let migrator = sqlx::migrate::Migrator::new(migrations_path)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load migrations: {e}")))?;

    migrator
        .run(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to run migrations: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transitions_queued_to_running() {
        let repo = RunsRepository::new(None)
            .await
            .expect("in-memory repository must initialize");
        let id = Uuid::new_v4();
        repo.create_run(
            id,
            "u1",
            "prompt".to_string(),
            Some(RunMode::GenerateFromScratch),
            Vec::new(),
        )
        .await
        .expect("run creation must work");

        assert!(repo.mark_running(id).await.expect("mark running must work"));
        let run = repo
            .get_run(id)
            .await
            .expect("fetch run must work")
            .expect("run must exist");
        assert_eq!(run.status, RunStatus::Running);
    }
}
