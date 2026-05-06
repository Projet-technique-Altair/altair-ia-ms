mod cloud_tasks;

use uuid::Uuid;

use crate::{error::AppError, services::run_processor::RunProcessor};

pub use cloud_tasks::{CloudTasksClient, CloudTasksConfig};

#[derive(Clone)]
enum QueueMode {
    Local,
    CloudTasks(CloudTasksClient),
}

#[derive(Clone)]
pub struct QueueClient {
    mode: QueueMode,
    run_processor: RunProcessor,
}

impl QueueClient {
    pub async fn new(
        cloud_tasks_enabled: bool,
        cloud_tasks_cfg: Option<CloudTasksConfig>,
        run_processor: RunProcessor,
    ) -> Result<Self, AppError> {
        let mode = if cloud_tasks_enabled {
            let cfg = cloud_tasks_cfg.ok_or_else(|| {
                AppError::Internal(
                    "Cloud Tasks is enabled but CLOUD_TASKS_* config is incomplete".to_string(),
                )
            })?;

            let client = CloudTasksClient::new(cfg)
                .await
                .map_err(|e| AppError::Internal(format!("failed to init CloudTasksClient: {e}")))?;
            QueueMode::CloudTasks(client)
        } else {
            QueueMode::Local
        };

        Ok(Self {
            mode,
            run_processor,
        })
    }

    pub async fn enqueue_process_run(&self, request_id: Uuid) -> Result<(), AppError> {
        self.enqueue_process_run_delayed(request_id, 0).await
    }

    pub async fn enqueue_process_run_delayed(
        &self,
        request_id: Uuid,
        delay_seconds: u64,
    ) -> Result<(), AppError> {
        match &self.mode {
            QueueMode::Local => {
                let processor = self.run_processor.clone();
                tokio::spawn(async move {
                    if delay_seconds > 0 {
                        tokio::time::sleep(std::time::Duration::from_secs(delay_seconds)).await;
                    }
                    processor.process_run(request_id).await;
                });
                Ok(())
            }
            QueueMode::CloudTasks(client) => client
                .enqueue_run(request_id, delay_seconds)
                .await
                .map_err(|e| AppError::Internal(format!("failed to enqueue Cloud Task: {e}"))),
        }
    }
}
