use std::time::Instant;

use uuid::Uuid;

use crate::services::{
    anthropic::AnthropicService,
    gemini::GeminiService,
    llm::{
        truncate_for_log, LabGenerationInput, LabGenerationOutput, LlmError, LlmErrorKind,
        TokenCountEstimate,
    },
    llm_config::{
        CLAUDE_MAX_ATTEMPTS_DEFAULT, GEMINI_MAX_ATTEMPTS_DEFAULT,
        LLM_ATTEMPT_TIMEOUT_SECONDS_DEFAULT, MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS,
    },
};

#[derive(Clone, Debug)]
pub struct LlmFallbackConfig {
    pub gemini_max_attempts: u8,
    pub claude_max_attempts: u8,
    pub attempt_timeout_seconds: u64,
}

impl LlmFallbackConfig {
    pub fn new(
        gemini_max_attempts: u8,
        claude_max_attempts: u8,
        attempt_timeout_seconds: u64,
    ) -> Self {
        Self {
            gemini_max_attempts: gemini_max_attempts.max(1),
            claude_max_attempts: claude_max_attempts.max(1),
            attempt_timeout_seconds: attempt_timeout_seconds.max(1),
        }
    }
}

impl Default for LlmFallbackConfig {
    fn default() -> Self {
        Self::new(
            GEMINI_MAX_ATTEMPTS_DEFAULT,
            CLAUDE_MAX_ATTEMPTS_DEFAULT,
            LLM_ATTEMPT_TIMEOUT_SECONDS_DEFAULT,
        )
    }
}

#[derive(Clone)]
pub struct FallbackLlmClient {
    gemini: GeminiService,
    claude: AnthropicService,
    config: LlmFallbackConfig,
}

impl FallbackLlmClient {
    pub fn new(gemini: GeminiService, claude: AnthropicService, config: LlmFallbackConfig) -> Self {
        Self {
            gemini,
            claude,
            config,
        }
    }

    pub fn primary_model_name(&self) -> &str {
        self.gemini.model_name()
    }

    pub async fn count_tokens(
        &self,
        run_id: Uuid,
        system: &str,
        user_message: &str,
    ) -> Result<TokenCountEstimate, LlmError> {
        self.gemini.count_tokens(run_id, system, user_message).await
    }

    pub async fn generate_lab_files(
        &self,
        input: &LabGenerationInput,
    ) -> Result<LabGenerationOutput, LlmError> {
        for attempt in 1..=self.config.gemini_max_attempts {
            let started = Instant::now();
            let result = self
                .gemini
                .generate_lab_files_attempt(input, self.config.attempt_timeout_seconds)
                .await;

            match result {
                Ok(output) => {
                    log_llm_attempt_success(
                        "gemini",
                        attempt,
                        &input.mode,
                        input.run_id,
                        false,
                        false,
                        started,
                    );
                    return Ok(output);
                }
                Err(error) => {
                    let fallback_allowed = is_gemini_overloaded_error(&error);
                    let fallback_triggered =
                        fallback_allowed && attempt >= self.config.gemini_max_attempts;
                    log_llm_attempt_failure(
                        "gemini",
                        attempt,
                        &input.mode,
                        input.run_id,
                        fallback_allowed,
                        fallback_triggered,
                        started,
                        &error,
                    );

                    if !fallback_allowed {
                        return Err(generic_ai_temporarily_unavailable());
                    }
                }
            }
        }

        for attempt in 1..=self.config.claude_max_attempts {
            let started = Instant::now();
            let result = self
                .claude
                .generate_lab_files_attempt(input, self.config.attempt_timeout_seconds)
                .await;

            match result {
                Ok(mut output) => {
                    output.used_fallback = true;
                    log_llm_attempt_success(
                        "anthropic",
                        attempt,
                        &input.mode,
                        input.run_id,
                        false,
                        false,
                        started,
                    );
                    return Ok(output);
                }
                Err(error) => {
                    log_llm_attempt_failure(
                        "anthropic",
                        attempt,
                        &input.mode,
                        input.run_id,
                        false,
                        false,
                        started,
                        &error,
                    );
                }
            }
        }

        Err(generic_ai_temporarily_unavailable())
    }

    pub async fn advise(
        &self,
        prompt: &str,
        mode: &str,
        request_id: Uuid,
    ) -> Result<String, LlmError> {
        for attempt in 1..=self.config.gemini_max_attempts {
            let started = Instant::now();
            let result = self
                .gemini
                .advise_attempt(prompt, request_id, self.config.attempt_timeout_seconds)
                .await;

            match result {
                Ok(output) => {
                    log_llm_attempt_success(
                        "gemini", attempt, mode, request_id, false, false, started,
                    );
                    return Ok(output);
                }
                Err(error) => {
                    let fallback_allowed = is_gemini_overloaded_error(&error);
                    let fallback_triggered =
                        fallback_allowed && attempt >= self.config.gemini_max_attempts;
                    log_llm_attempt_failure(
                        "gemini",
                        attempt,
                        mode,
                        request_id,
                        fallback_allowed,
                        fallback_triggered,
                        started,
                        &error,
                    );

                    if !fallback_allowed {
                        return Err(generic_ai_temporarily_unavailable());
                    }
                }
            }
        }

        for attempt in 1..=self.config.claude_max_attempts {
            let started = Instant::now();
            let result = self
                .claude
                .advise_attempt(prompt, request_id, self.config.attempt_timeout_seconds)
                .await;

            match result {
                Ok(output) => {
                    log_llm_attempt_success(
                        "anthropic",
                        attempt,
                        mode,
                        request_id,
                        false,
                        false,
                        started,
                    );
                    return Ok(output);
                }
                Err(error) => {
                    log_llm_attempt_failure(
                        "anthropic",
                        attempt,
                        mode,
                        request_id,
                        false,
                        false,
                        started,
                        &error,
                    );
                }
            }
        }

        Err(generic_ai_temporarily_unavailable())
    }
}

pub fn is_gemini_overloaded_error(error: &LlmError) -> bool {
    if matches!(error.status, Some(429 | 503)) {
        return true;
    }

    let message = error.message.to_ascii_lowercase();
    [
        "resource_exhausted",
        "service unavailable",
        "model overloaded",
        "provider overloaded",
        "overloaded",
        "please try again later",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn generic_ai_temporarily_unavailable() -> LlmError {
    LlmError::new(
        LlmErrorKind::TemporarilyUnavailable,
        "AI_TEMPORARILY_UNAVAILABLE",
    )
    .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS))
}

fn log_llm_attempt_success(
    provider: &'static str,
    attempt: u8,
    mode: &str,
    request_id: Uuid,
    fallback_allowed: bool,
    fallback_triggered: bool,
    started: Instant,
) {
    tracing::info!(
        llm.provider = provider,
        llm.attempt = attempt,
        llm.mode = mode,
        llm.status = "success",
        llm.error_type = "",
        llm.http_status = 0,
        llm.fallback_allowed = fallback_allowed,
        llm.fallback_triggered = fallback_triggered,
        llm.duration_ms = started.elapsed().as_millis() as u64,
        request_id = %request_id,
        "llm attempt completed"
    );
}

fn log_llm_attempt_failure(
    provider: &'static str,
    attempt: u8,
    mode: &str,
    request_id: Uuid,
    fallback_allowed: bool,
    fallback_triggered: bool,
    started: Instant,
    error: &LlmError,
) {
    tracing::warn!(
        llm.provider = provider,
        llm.attempt = attempt,
        llm.mode = mode,
        llm.status = "failure",
        llm.error_type = error.kind.as_str(),
        llm.http_status = error.status.unwrap_or_default(),
        llm.fallback_allowed = fallback_allowed,
        llm.fallback_triggered = fallback_triggered,
        llm.duration_ms = started.elapsed().as_millis() as u64,
        llm.error_detail = %truncate_for_log(&error.message, 500),
        request_id = %request_id,
        "llm attempt failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_overload_statuses_enable_fallback() {
        for status in [429, 503] {
            let error = LlmError {
                kind: LlmErrorKind::ModelUnavailable,
                message: "provider error".to_string(),
                status: Some(status),
                retry_after_seconds: None,
            };

            assert!(is_gemini_overloaded_error(&error));
        }
    }

    #[test]
    fn gemini_overload_messages_enable_fallback() {
        for message in [
            "RESOURCE_EXHAUSTED",
            "Service Unavailable",
            "model overloaded",
            "provider overloaded",
            "please try again later",
        ] {
            let error = LlmError::new(LlmErrorKind::ModelUnavailable, message);

            assert!(is_gemini_overloaded_error(&error));
        }
    }

    #[test]
    fn non_overload_errors_do_not_enable_fallback() {
        for message in [
            "GEMINI_API_KEY is not configured",
            "invalid JSON",
            "schema validation failed",
            "safety refusal",
        ] {
            let error = LlmError::new(LlmErrorKind::InvalidRequest, message);

            assert!(!is_gemini_overloaded_error(&error));
        }
    }

    #[test]
    fn final_error_is_generic() {
        let error = generic_ai_temporarily_unavailable();

        assert_eq!(error.kind, LlmErrorKind::TemporarilyUnavailable);
        assert_eq!(error.message, "AI_TEMPORARILY_UNAVAILABLE");
    }
}
