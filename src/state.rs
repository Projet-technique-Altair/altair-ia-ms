use crate::{
    config::{AppConfig, LlmProvider},
    error::AppError,
    queue::{CloudTasksConfig, QueueClient},
    repository::{run_uploads_repository::RunUploadsRepository, runs_repository::RunsRepository},
    services::{
        anthropic::AnthropicService,
        gemini::GeminiService,
        llm::LlmClient,
        llm_fallback::{FallbackLlmClient, LlmFallbackConfig},
        oidc::OidcVerifier,
        run_processor::RunProcessor,
        storage::StorageService,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub runs_repo: RunsRepository,
    pub run_uploads_repo: RunUploadsRepository,
    pub storage: StorageService,
    pub llm: LlmClient,
    pub run_processor: RunProcessor,
    pub queue: QueueClient,
    pub oidc_verifier: Option<OidcVerifier>,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self, AppError> {
        let runs_repo = RunsRepository::new(config.database_url.as_deref()).await?;
        let run_uploads_repo = RunUploadsRepository::new(config.database_url.as_deref()).await?;
        let storage = StorageService::new(
            config.bucket_name.clone(),
            config.signed_url_ttl_seconds,
            config.gcs_signed_url_mode.clone(),
            config.gcs_signing_service_account.clone(),
            config.local_storage_dir.clone(),
            config.public_base_url.clone(),
        )
        .await?;

        let llm = match &config.llm_provider {
            LlmProvider::Anthropic => LlmClient::Anthropic(AnthropicService::new(
                config.anthropic_api_key.clone(),
                config.anthropic_base_url.clone(),
                config.anthropic_model.clone(),
            )),
            LlmProvider::Gemini => {
                let gemini = GeminiService::new(
                    config.gemini_api_key.clone(),
                    config.gemini_base_url.clone(),
                    config.gemini_model.clone(),
                    config.gemini_thinking_level.clone(),
                );

                if config.llm_fallback_enabled {
                    LlmClient::GeminiWithClaudeFallback(FallbackLlmClient::new(
                        gemini,
                        AnthropicService::new(
                            config.anthropic_api_key.clone(),
                            config.anthropic_base_url.clone(),
                            config.anthropic_model.clone(),
                        ),
                        LlmFallbackConfig::new(
                            config.gemini_max_attempts,
                            config.claude_max_attempts,
                            config.llm_attempt_timeout_seconds,
                        ),
                    ))
                } else {
                    LlmClient::Gemini(gemini)
                }
            }
        };

        tracing::info!(
            llm_provider = llm.provider_name(),
            llm_model = llm.model_name(),
            llm_fallback_enabled = matches!(&llm, LlmClient::GeminiWithClaudeFallback(_)),
            gemini_max_attempts = config.gemini_max_attempts,
            claude_max_attempts = config.claude_max_attempts,
            llm_attempt_timeout_seconds = config.llm_attempt_timeout_seconds,
            "llm client initialized"
        );

        let run_processor = RunProcessor::new(
            runs_repo.clone(),
            llm.clone(),
            storage.clone(),
            config.run_process_timeout_seconds,
            config.run_process_max_attempts,
        );

        let cloud_tasks_cfg = if config.cloud_tasks_enabled {
            Some(CloudTasksConfig {
                project_id: config.cloud_tasks_project_id.clone().ok_or_else(|| {
                    AppError::Internal("CLOUD_TASKS_PROJECT_ID is required".to_string())
                })?,
                location: config.cloud_tasks_location.clone().ok_or_else(|| {
                    AppError::Internal("CLOUD_TASKS_LOCATION is required".to_string())
                })?,
                queue: config.cloud_tasks_queue.clone().ok_or_else(|| {
                    AppError::Internal("CLOUD_TASKS_QUEUE is required".to_string())
                })?,
                max_enqueue_attempts: config.cloud_tasks_enqueue_max_attempts,
                worker_target_base_url: config.worker_target_base_url.clone().ok_or_else(|| {
                    AppError::Internal("WORKER_TARGET_BASE_URL is required".to_string())
                })?,
                worker_oidc_service_account: config
                    .worker_oidc_service_account
                    .clone()
                    .ok_or_else(|| {
                        AppError::Internal("WORKER_OIDC_SERVICE_ACCOUNT is required".to_string())
                    })?,
                worker_oidc_audience: config.worker_oidc_audience.clone(),
            })
        } else {
            None
        };

        let queue = QueueClient::new(
            config.cloud_tasks_enabled,
            cloud_tasks_cfg,
            run_processor.clone(),
        )
        .await?;

        let oidc_verifier = if config.cloud_tasks_enabled {
            let expected_audience = config
                .worker_oidc_audience
                .clone()
                .or(config.worker_target_base_url.clone())
                .ok_or_else(|| {
                    AppError::Internal(
                        "WORKER_OIDC_AUDIENCE or WORKER_TARGET_BASE_URL is required".to_string(),
                    )
                })?;

            let allowed_sa = config.worker_oidc_service_account.clone().ok_or_else(|| {
                AppError::Internal("WORKER_OIDC_SERVICE_ACCOUNT is required".to_string())
            })?;

            Some(OidcVerifier::new(
                config.oidc_tokeninfo_url.clone(),
                expected_audience,
                allowed_sa,
            ))
        } else {
            None
        };

        Ok(Self {
            config,
            runs_repo,
            run_uploads_repo,
            storage,
            llm,
            run_processor,
            queue,
            oidc_verifier,
        })
    }
}
