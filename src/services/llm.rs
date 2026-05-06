use serde::Serialize;
use uuid::Uuid;

use crate::services::{
    anthropic::AnthropicService,
    gemini::GeminiService,
    llm_config::{MAX_INPUT_TOKENS, TOKEN_BUDGET_WARNING},
    llm_fallback::FallbackLlmClient,
};

pub(crate) const NO_RESPONSE_TEXT: &str = "No response text from model";

#[derive(Debug, Clone, Serialize)]
pub struct LabGenerationInput {
    pub run_id: Uuid,
    pub mode: String,
    pub system: String,
    pub user_message: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UsageStats {
    pub estimated_input_tokens: Option<u64>,
    pub actual_input_tokens: Option<u64>,
    pub actual_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabGenerationOutput {
    pub raw_response: String,
    pub usage: UsageStats,
    pub provider: String,
    pub model: String,
    pub used_fallback: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenCountEstimate {
    pub input_tokens: u64,
    pub from_api: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    PromptTooLarge,
    ModelUnavailable,
    InvalidRequest,
    Unauthorized,
    Forbidden,
    Unprocessable,
    RateLimited,
    ServerError,
    Transport,
    Decode,
    EmptyResponse,
    TemporarilyUnavailable,
}

impl LlmErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromptTooLarge => "prompt_too_large",
            Self::ModelUnavailable => "model_unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Unprocessable => "unprocessable",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
            Self::Transport => "transport",
            Self::Decode => "decode",
            Self::EmptyResponse => "empty_response",
            Self::TemporarilyUnavailable => "temporarily_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmError {
    pub kind: LlmErrorKind,
    pub message: String,
    pub status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LlmError {}

impl LlmError {
    pub fn new(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            retry_after_seconds: None,
        }
    }

    pub fn with_status(mut self, status: reqwest::StatusCode) -> Self {
        self.status = Some(status.as_u16());
        self
    }

    pub fn with_retry_after(mut self, seconds: Option<u64>) -> Self {
        self.retry_after_seconds = seconds;
        self
    }
}

#[derive(Clone)]
pub enum LlmClient {
    Anthropic(AnthropicService),
    Gemini(GeminiService),
    GeminiWithClaudeFallback(FallbackLlmClient),
}

impl LlmClient {
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Anthropic(_) => "anthropic",
            Self::Gemini(_) => "gemini",
            Self::GeminiWithClaudeFallback(_) => "gemini",
        }
    }

    pub fn model_name(&self) -> &str {
        match self {
            Self::Anthropic(service) => service.model_name(),
            Self::Gemini(service) => service.model_name(),
            Self::GeminiWithClaudeFallback(service) => service.primary_model_name(),
        }
    }

    pub async fn count_tokens(
        &self,
        run_id: Uuid,
        system: &str,
        user_message: &str,
    ) -> Result<TokenCountEstimate, LlmError> {
        match self {
            Self::Anthropic(service) => service.count_tokens(run_id, system, user_message).await,
            Self::Gemini(service) => service.count_tokens(run_id, system, user_message).await,
            Self::GeminiWithClaudeFallback(service) => {
                service.count_tokens(run_id, system, user_message).await
            }
        }
    }

    pub fn enforce_token_budget(
        &self,
        run_id: Uuid,
        estimate: &TokenCountEstimate,
    ) -> Result<(), LlmError> {
        enforce_token_budget_for_model(run_id, self.model_name(), estimate)
    }

    pub async fn generate_lab_files(
        &self,
        input: &LabGenerationInput,
    ) -> Result<LabGenerationOutput, LlmError> {
        match self {
            Self::Anthropic(service) => service.generate_lab_files(input).await,
            Self::Gemini(service) => service.generate_lab_files(input).await,
            Self::GeminiWithClaudeFallback(service) => service.generate_lab_files(input).await,
        }
    }

    pub async fn generate_lab_files_with_preferred_provider(
        &self,
        input: &LabGenerationInput,
        preferred_provider: &str,
    ) -> Result<LabGenerationOutput, LlmError> {
        match self {
            Self::Anthropic(service) => service.generate_lab_files(input).await,
            Self::Gemini(service) => service.generate_lab_files(input).await,
            Self::GeminiWithClaudeFallback(service) => {
                service
                    .generate_lab_files_with_preferred_provider(input, preferred_provider)
                    .await
            }
        }
    }

    #[allow(dead_code)]
    pub async fn advise(&self, prompt: &str) -> anyhow::Result<String> {
        match self {
            Self::Anthropic(service) => service.advise(prompt).await,
            Self::Gemini(service) => service.advise(prompt).await,
            Self::GeminiWithClaudeFallback(service) => service
                .advise(prompt, "advise", Uuid::nil())
                .await
                .map_err(anyhow::Error::msg),
        }
    }

    pub async fn advise_with_context(
        &self,
        prompt: &str,
        mode: &str,
        request_id: Uuid,
    ) -> Result<String, LlmError> {
        match self {
            Self::Anthropic(service) => service
                .advise(prompt)
                .await
                .map_err(|e| LlmError::new(LlmErrorKind::ModelUnavailable, e.to_string())),
            Self::Gemini(service) => service
                .advise(prompt)
                .await
                .map_err(|e| LlmError::new(LlmErrorKind::ModelUnavailable, e.to_string())),
            Self::GeminiWithClaudeFallback(service) => {
                service.advise(prompt, mode, request_id).await
            }
        }
    }
}

pub fn estimate_tokens(input: &str) -> u64 {
    let chars = input.chars().count() as u64;
    chars.div_ceil(4).max(1)
}

pub(crate) fn enforce_token_budget_for_model(
    run_id: Uuid,
    model: &str,
    estimate: &TokenCountEstimate,
) -> Result<(), LlmError> {
    if estimate.input_tokens > MAX_INPUT_TOKENS {
        return Err(LlmError::new(
            LlmErrorKind::PromptTooLarge,
            format!(
                "Prompt too large: {} input tokens > max {}",
                estimate.input_tokens, MAX_INPUT_TOKENS
            ),
        ));
    }

    if estimate.input_tokens > TOKEN_BUDGET_WARNING {
        tracing::warn!(
            run_id = %run_id,
            stage = "pre_dispatch",
            model = %model,
            estimated_input_tokens = estimate.input_tokens,
            warning_threshold = TOKEN_BUDGET_WARNING,
            "token budget warning"
        );
    }

    Ok(())
}

pub(crate) fn local_advise_fallback(prompt: &str) -> String {
    format!("(local-fallback) Advice generated for prompt: {prompt}")
}

pub(crate) fn truncate_for_log(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    format!("{}...[truncated]", &value[..max_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimator_is_non_zero() {
        assert!(estimate_tokens("abc") >= 1);
    }
}
