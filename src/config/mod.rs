use crate::services::llm_config::{
    ANTHROPIC_DEFAULT_MODEL, CLAUDE_MAX_ATTEMPTS_DEFAULT, GEMINI_DEFAULT_MODEL,
    GEMINI_MAX_ATTEMPTS_DEFAULT, LLM_ATTEMPT_TIMEOUT_SECONDS_DEFAULT,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    Local,
    PseudoProd,
}

impl RuntimeMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "pseudo_prod" | "pseudo-prod" => Some(Self::PseudoProd),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LlmProvider {
    Anthropic,
    Gemini,
}

impl LlmProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            "gemini" | "google" => Some(Self::Gemini),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub runtime_mode: RuntimeMode,
    pub port: u16,
    pub bucket_name: String,
    pub signed_url_ttl_seconds: i64,
    pub require_creator_role: bool,
    pub llm_provider: LlmProvider,
    pub anthropic_api_key: Option<String>,
    pub anthropic_base_url: String,
    pub anthropic_model: String,
    pub gemini_api_key: Option<String>,
    pub gemini_base_url: String,
    pub gemini_model: String,
    pub gemini_thinking_level: Option<String>,
    pub llm_fallback_enabled: bool,
    pub gemini_max_attempts: u8,
    pub claude_max_attempts: u8,
    pub llm_attempt_timeout_seconds: u64,
    pub cloud_tasks_enabled: bool,
    pub internal_worker_token: Option<String>,
    pub gcs_signed_url_mode: String,
    pub gcs_signing_service_account: Option<String>,
    pub local_storage_dir: String,
    pub public_base_url: String,
    pub cloud_tasks_project_id: Option<String>,
    pub cloud_tasks_location: Option<String>,
    pub cloud_tasks_queue: Option<String>,
    pub cloud_tasks_enqueue_max_attempts: u32,
    pub worker_target_base_url: Option<String>,
    pub worker_oidc_service_account: Option<String>,
    pub worker_oidc_audience: Option<String>,
    pub oidc_tokeninfo_url: String,
    pub run_process_timeout_seconds: u64,
    pub run_process_max_attempts: u8,
    pub database_url: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, String> {
        let runtime_mode_raw =
            std::env::var("IA_RUNTIME_MODE").unwrap_or_else(|_| "local".to_string());
        let runtime_mode = RuntimeMode::parse(&runtime_mode_raw).ok_or_else(|| {
            format!(
                "invalid IA_RUNTIME_MODE `{}` (allowed: local, pseudo_prod)",
                runtime_mode_raw
            )
        })?;

        let port = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3011);

        let bucket_name =
            std::env::var("LAB_IA_BUCKET").unwrap_or_else(|_| "altair-ia-labs".to_string());

        let signed_url_ttl_seconds = std::env::var("SIGNED_URL_TTL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(600);

        let require_creator_role = std::env::var("REQUIRE_CREATOR_ROLE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);

        let llm_provider_raw =
            std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "gemini".to_string());
        let llm_provider = LlmProvider::parse(&llm_provider_raw).ok_or_else(|| {
            format!(
                "invalid LLM_PROVIDER `{}` (allowed: anthropic, gemini)",
                llm_provider_raw
            )
        })?;

        let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY").ok();
        let anthropic_base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
        let anthropic_model = std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| ANTHROPIC_DEFAULT_MODEL.to_string());

        let gemini_api_key = std::env::var("GEMINI_API_KEY").ok();
        let gemini_base_url = std::env::var("GEMINI_BASE_URL")
            .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".to_string());
        let gemini_model =
            std::env::var("GEMINI_MODEL").unwrap_or_else(|_| GEMINI_DEFAULT_MODEL.to_string());
        let gemini_thinking_level = std::env::var("GEMINI_THINKING_LEVEL")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        if let Some(level) = gemini_thinking_level.as_deref() {
            if !matches!(level, "low" | "high") {
                return Err(format!(
                    "invalid GEMINI_THINKING_LEVEL `{}` (allowed: low, high)",
                    level
                ));
            }
        }

        let llm_fallback_enabled = parse_bool_env("LLM_FALLBACK_ENABLED", true);
        let gemini_max_attempts =
            parse_u8_env("GEMINI_MAX_ATTEMPTS", GEMINI_MAX_ATTEMPTS_DEFAULT, 1, 10);
        let claude_max_attempts =
            parse_u8_env("CLAUDE_MAX_ATTEMPTS", CLAUDE_MAX_ATTEMPTS_DEFAULT, 1, 10);
        let llm_attempt_timeout_seconds = parse_u64_env(
            "LLM_ATTEMPT_TIMEOUT_SECONDS",
            LLM_ATTEMPT_TIMEOUT_SECONDS_DEFAULT,
            1,
            600,
        );

        let cloud_tasks_enabled = std::env::var("CLOUD_TASKS_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let internal_worker_token = std::env::var("INTERNAL_WORKER_TOKEN").ok();

        let gcs_signed_url_mode = std::env::var("GCS_SIGNED_URL_MODE")
            .unwrap_or_else(|_| "iam_signblob".to_string())
            .to_ascii_lowercase();

        let gcs_signing_service_account = std::env::var("GCS_SIGNING_SERVICE_ACCOUNT").ok();
        let local_storage_dir = std::env::var("LOCAL_STORAGE_DIR")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| ".local-storage".to_string());
        let public_base_url = std::env::var("PUBLIC_BASE_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| format!("http://localhost:{port}"));
        let cloud_tasks_project_id = std::env::var("CLOUD_TASKS_PROJECT_ID").ok();
        let cloud_tasks_location = std::env::var("CLOUD_TASKS_LOCATION").ok();
        let cloud_tasks_queue = std::env::var("CLOUD_TASKS_QUEUE").ok();
        let cloud_tasks_enqueue_max_attempts = std::env::var("CLOUD_TASKS_ENQUEUE_MAX_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v.clamp(1, 10))
            .unwrap_or(3);
        let worker_target_base_url = std::env::var("WORKER_TARGET_BASE_URL").ok();
        let worker_oidc_service_account = std::env::var("WORKER_OIDC_SERVICE_ACCOUNT").ok();
        let worker_oidc_audience = std::env::var("WORKER_OIDC_AUDIENCE").ok();
        let oidc_tokeninfo_url = std::env::var("OIDC_TOKENINFO_URL")
            .unwrap_or_else(|_| "https://oauth2.googleapis.com/tokeninfo".to_string());
        let run_process_timeout_seconds = std::env::var("RUN_PROCESS_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v.clamp(30, 3600))
            .unwrap_or(700);
        let run_process_max_attempts = 1;
        let database_url = std::env::var("DATABASE_URL").ok();

        let cfg = Self {
            runtime_mode,
            port,
            bucket_name,
            signed_url_ttl_seconds,
            require_creator_role,
            llm_provider,
            anthropic_api_key,
            anthropic_base_url,
            anthropic_model,
            gemini_api_key,
            gemini_base_url,
            gemini_model,
            gemini_thinking_level,
            llm_fallback_enabled,
            gemini_max_attempts,
            claude_max_attempts,
            llm_attempt_timeout_seconds,
            cloud_tasks_enabled,
            internal_worker_token,
            gcs_signed_url_mode,
            gcs_signing_service_account,
            local_storage_dir,
            public_base_url,
            cloud_tasks_project_id,
            cloud_tasks_location,
            cloud_tasks_queue,
            cloud_tasks_enqueue_max_attempts,
            worker_target_base_url,
            worker_oidc_service_account,
            worker_oidc_audience,
            oidc_tokeninfo_url,
            run_process_timeout_seconds,
            run_process_max_attempts,
            database_url,
        };

        cfg.validate_mode_consistency()?;
        Ok(cfg)
    }

    fn validate_mode_consistency(&self) -> Result<(), String> {
        let has_database = self
            .database_url
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_anthropic_key = self
            .anthropic_api_key
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_gemini_key = self
            .gemini_api_key
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_worker_base_url = self
            .worker_target_base_url
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_worker_oidc_sa = self
            .worker_oidc_service_account
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_gcs_signing_sa = self
            .gcs_signing_service_account
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        match self.runtime_mode {
            RuntimeMode::Local => {
                if self.cloud_tasks_enabled {
                    return Err(
                        "IA_RUNTIME_MODE=local requires CLOUD_TASKS_ENABLED=false".to_string()
                    );
                }
                if self.gcs_signed_url_mode != "mock" {
                    return Err(
                        "IA_RUNTIME_MODE=local requires GCS_SIGNED_URL_MODE=mock".to_string()
                    );
                }
                if !has_database {
                    return Err("IA_RUNTIME_MODE=local requires DATABASE_URL to be set".to_string());
                }
            }
            RuntimeMode::PseudoProd => {
                if !self.cloud_tasks_enabled {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod requires CLOUD_TASKS_ENABLED=true".to_string(),
                    );
                }
                if self.gcs_signed_url_mode != "iam_signblob" {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod requires GCS_SIGNED_URL_MODE=iam_signblob"
                            .to_string(),
                    );
                }
                if !has_database {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod requires DATABASE_URL to be set".to_string(),
                    );
                }
                match self.llm_provider {
                    LlmProvider::Anthropic if !has_anthropic_key => {
                        return Err(
                            "IA_RUNTIME_MODE=pseudo_prod with LLM_PROVIDER=anthropic requires ANTHROPIC_API_KEY to be set"
                                .to_string(),
                        );
                    }
                    LlmProvider::Gemini if !has_gemini_key => {
                        return Err(
                            "IA_RUNTIME_MODE=pseudo_prod with LLM_PROVIDER=gemini requires GEMINI_API_KEY to be set"
                                .to_string(),
                        );
                    }
                    _ => {}
                }
                if matches!(self.llm_provider, LlmProvider::Gemini)
                    && self.llm_fallback_enabled
                    && !has_anthropic_key
                {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod with Gemini fallback enabled requires ANTHROPIC_API_KEY to be set"
                            .to_string(),
                    );
                }
                if !has_worker_base_url {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod requires WORKER_TARGET_BASE_URL to be set"
                            .to_string(),
                    );
                }
                if !has_worker_oidc_sa {
                    return Err("IA_RUNTIME_MODE=pseudo_prod requires WORKER_OIDC_SERVICE_ACCOUNT to be set".to_string());
                }
                if !has_gcs_signing_sa {
                    return Err("IA_RUNTIME_MODE=pseudo_prod requires GCS_SIGNING_SERVICE_ACCOUNT to be set".to_string());
                }
                if !self.require_creator_role {
                    return Err(
                        "IA_RUNTIME_MODE=pseudo_prod requires REQUIRE_CREATOR_ROLE=true"
                            .to_string(),
                    );
                }
            }
        }

        Ok(())
    }
}

fn parse_bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes"
            )
        })
        .unwrap_or(default)
}

fn parse_u8_env(name: &str, default: u8, min: u8, max: u8) -> u8 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default)
}

fn parse_u64_env(name: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default)
}
