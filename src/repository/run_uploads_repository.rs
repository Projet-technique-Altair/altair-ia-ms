use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::AppError;

type InMemoryRunUploads = Arc<RwLock<HashMap<(Uuid, String), Vec<RunUploadRecord>>>>;

#[derive(Clone)]
enum RunUploadsStore {
    InMemory(InMemoryRunUploads),
    Postgres(PgPool),
}

#[derive(Clone)]
pub struct RunUploadsRepository {
    store: RunUploadsStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunUploadRecord {
    pub run_id: Uuid,
    pub user_id: String,
    pub object_key: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct RunUploadRow {
    run_id: Uuid,
    user_id: String,
    object_key: String,
    status: String,
    created_at: DateTime<Utc>,
    consumed_at: Option<DateTime<Utc>>,
}

impl From<RunUploadRow> for RunUploadRecord {
    fn from(row: RunUploadRow) -> Self {
        Self {
            run_id: row.run_id,
            user_id: row.user_id,
            object_key: row.object_key,
            status: row.status,
            created_at: row.created_at,
            consumed_at: row.consumed_at,
        }
    }
}

impl RunUploadsRepository {
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

            return Ok(Self {
                store: RunUploadsStore::Postgres(pool),
            });
        }

        Ok(Self {
            store: RunUploadsStore::InMemory(Arc::new(RwLock::new(HashMap::new()))),
        })
    }

    pub async fn register_uploaded_objects(
        &self,
        run_id: Uuid,
        user_id: &str,
        object_keys: &[String],
    ) -> Result<(), AppError> {
        if object_keys.is_empty() {
            return Ok(());
        }

        match &self.store {
            RunUploadsStore::InMemory(store) => {
                let mut lock = store.write().await;
                let key = (run_id, user_id.to_string());
                let records = lock.entry(key).or_default();
                for object_key in object_keys {
                    if let Some(record) = records.iter_mut().find(|r| r.object_key == *object_key) {
                        record.status = "uploaded".to_string();
                        record.consumed_at = None;
                    } else {
                        records.push(RunUploadRecord {
                            run_id,
                            user_id: user_id.to_string(),
                            object_key: object_key.clone(),
                            status: "uploaded".to_string(),
                            created_at: Utc::now(),
                            consumed_at: None,
                        });
                    }
                }
                Ok(())
            }
            RunUploadsStore::Postgres(pool) => {
                for object_key in object_keys {
                    sqlx::query(
                        r#"
                        INSERT INTO ia_run_uploads (run_id, user_id, object_key, status, created_at, consumed_at)
                        VALUES ($1, $2, $3, 'uploaded', NOW(), NULL)
                        ON CONFLICT (run_id, user_id, object_key)
                        DO UPDATE SET
                            status = 'uploaded',
                            consumed_at = NULL
                        "#,
                    )
                    .bind(run_id)
                    .bind(user_id)
                    .bind(object_key)
                    .execute(pool)
                    .await
                    .map_err(|e| {
                        AppError::Internal(format!("failed to register upload object: {e}"))
                    })?;
                }
                Ok(())
            }
        }
    }

    pub async fn list_uploaded_object_keys(
        &self,
        run_id: Uuid,
        user_id: &str,
    ) -> Result<Vec<String>, AppError> {
        match &self.store {
            RunUploadsStore::InMemory(store) => {
                let lock = store.read().await;
                let out = lock
                    .get(&(run_id, user_id.to_string()))
                    .map(|records| {
                        records
                            .iter()
                            .filter(|record| record.status == "uploaded")
                            .map(|record| record.object_key.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(out)
            }
            RunUploadsStore::Postgres(pool) => {
                let rows = sqlx::query_as::<_, RunUploadRow>(
                    r#"
                    SELECT run_id, user_id, object_key, status, created_at, consumed_at
                    FROM ia_run_uploads
                    WHERE run_id = $1 AND user_id = $2 AND status = 'uploaded'
                    ORDER BY created_at ASC
                    "#,
                )
                .bind(run_id)
                .bind(user_id)
                .fetch_all(pool)
                .await
                .map_err(|e| AppError::Internal(format!("failed to list uploaded objects: {e}")))?;

                Ok(rows.into_iter().map(|row| row.object_key).collect())
            }
        }
    }

    pub async fn mark_consumed(
        &self,
        run_id: Uuid,
        user_id: &str,
        object_keys: &[String],
    ) -> Result<(), AppError> {
        if object_keys.is_empty() {
            return Ok(());
        }

        match &self.store {
            RunUploadsStore::InMemory(store) => {
                let mut lock = store.write().await;
                if let Some(records) = lock.get_mut(&(run_id, user_id.to_string())) {
                    for record in records.iter_mut() {
                        if object_keys.iter().any(|k| k == &record.object_key) {
                            record.status = "consumed".to_string();
                            record.consumed_at = Some(Utc::now());
                        }
                    }
                }
                Ok(())
            }
            RunUploadsStore::Postgres(pool) => {
                for object_key in object_keys {
                    sqlx::query(
                        r#"
                        UPDATE ia_run_uploads
                        SET status = 'consumed', consumed_at = NOW()
                        WHERE run_id = $1 AND user_id = $2 AND object_key = $3
                        "#,
                    )
                    .bind(run_id)
                    .bind(user_id)
                    .bind(object_key)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Internal(format!("failed to mark consumed: {e}")))?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunUploadsRepository;
    use uuid::Uuid;

    #[tokio::test]
    async fn register_list_and_consume_in_memory() {
        let repo = RunUploadsRepository::new(None)
            .await
            .expect("in memory repo must init");
        let run_id = Uuid::new_v4();
        let user_id = "u1";
        let files = vec![
            "uploads/r1/a.zip".to_string(),
            "uploads/r1/src/main.py".to_string(),
        ];

        repo.register_uploaded_objects(run_id, user_id, &files)
            .await
            .expect("register must work");
        let listed = repo
            .list_uploaded_object_keys(run_id, user_id)
            .await
            .expect("list must work");
        assert_eq!(listed.len(), 2);

        repo.mark_consumed(run_id, user_id, &[files[0].clone()])
            .await
            .expect("consume must work");
        let listed_after = repo
            .list_uploaded_object_keys(run_id, user_id)
            .await
            .expect("list must work");
        assert_eq!(listed_after, vec![files[1].clone()]);
    }
}
