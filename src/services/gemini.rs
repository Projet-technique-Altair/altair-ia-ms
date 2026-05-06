use serde_json::json;
use uuid::Uuid;

use crate::services::{
    llm::{
        estimate_tokens, local_advise_fallback, LabGenerationInput, LabGenerationOutput, LlmError,
        LlmErrorKind, TokenCountEstimate, UsageStats, NO_RESPONSE_TEXT,
    },
    llm_config::{
        MAX_OUTPUT_TOKENS, MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS, PER_ATTEMPT_TIMEOUT_SECONDS,
        RETRY_MAX_ATTEMPTS,
    },
    llm_http::{post_json_with_retry, JsonPostRequest},
};

const EXTRA_NO_RETRY_STATUSES: [u16; 1] = [404];

#[derive(Clone)]
pub struct GeminiService {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    thinking_level: Option<String>,
}

impl GeminiService {
    pub fn new(
        api_key: Option<String>,
        base_url: String,
        model: String,
        thinking_level: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
            model,
            thinking_level,
        }
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    pub async fn count_tokens(
        &self,
        _run_id: Uuid,
        system: &str,
        user_message: &str,
    ) -> Result<TokenCountEstimate, LlmError> {
        let estimated = estimate_tokens(system) + estimate_tokens(user_message);
        Ok(TokenCountEstimate {
            input_tokens: estimated,
            from_api: false,
        })
    }

    pub async fn generate_lab_files(
        &self,
        input: &LabGenerationInput,
    ) -> Result<LabGenerationOutput, LlmError> {
        self.generate_lab_files_with_http_options(
            input,
            RETRY_MAX_ATTEMPTS,
            PER_ATTEMPT_TIMEOUT_SECONDS,
        )
        .await
    }

    pub async fn generate_lab_files_attempt(
        &self,
        input: &LabGenerationInput,
        attempt_timeout_seconds: u64,
    ) -> Result<LabGenerationOutput, LlmError> {
        self.generate_lab_files_with_http_options(input, 1, attempt_timeout_seconds)
            .await
    }

    async fn generate_lab_files_with_http_options(
        &self,
        input: &LabGenerationInput,
        max_attempts: u8,
        attempt_timeout_seconds: u64,
    ) -> Result<LabGenerationOutput, LlmError> {
        let key = self.api_key.clone().ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::ModelUnavailable,
                "GEMINI_API_KEY is not configured",
            )
            .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS))
        })?;

        let body = self.build_generate_body(&input.system, &input.user_message);
        let payload = self
            .generate_content_call_with_retry(
                input.run_id,
                &key,
                body,
                max_attempts,
                attempt_timeout_seconds,
            )
            .await?;
        let answer = extract_candidate_text(&payload)
            .ok_or_else(|| LlmError::new(LlmErrorKind::EmptyResponse, NO_RESPONSE_TEXT))?;

        Ok(LabGenerationOutput {
            raw_response: answer,
            usage: extract_usage(&payload),
            provider: "gemini".to_string(),
            model: self.model.clone(),
            used_fallback: false,
        })
    }

    pub async fn advise(&self, prompt: &str) -> anyhow::Result<String> {
        if self.api_key.is_none() {
            return Ok(local_advise_fallback(prompt));
        }

        let key = self.api_key.clone().unwrap_or_default();
        let body = self.build_generate_body("", prompt);
        let payload = self
            .generate_content_call_with_retry(
                Uuid::nil(),
                &key,
                body,
                RETRY_MAX_ATTEMPTS,
                PER_ATTEMPT_TIMEOUT_SECONDS,
            )
            .await
            .map_err(anyhow::Error::msg)?;

        Ok(extract_candidate_text(&payload).unwrap_or_else(|| NO_RESPONSE_TEXT.to_string()))
    }

    pub async fn advise_attempt(
        &self,
        prompt: &str,
        request_id: Uuid,
        attempt_timeout_seconds: u64,
    ) -> Result<String, LlmError> {
        let key = self.api_key.clone().ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::ModelUnavailable,
                "GEMINI_API_KEY is not configured",
            )
            .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS))
        })?;

        let body = self.build_generate_body("", prompt);
        let payload = self
            .generate_content_call_with_retry(request_id, &key, body, 1, attempt_timeout_seconds)
            .await?;

        Ok(extract_candidate_text(&payload).unwrap_or_else(|| NO_RESPONSE_TEXT.to_string()))
    }

    fn build_generate_body(&self, system: &str, user_message: &str) -> serde_json::Value {
        let mut generation_config = json!({
            "maxOutputTokens": MAX_OUTPUT_TOKENS
        });

        if let Some(level) = self.thinking_level.as_deref() {
            generation_config["thinkingConfig"] = json!({
                "thinkingLevel": level
            });
        }

        let mut body = json!({
            "contents": [{
                "parts": [{"text": user_message}]
            }],
            "generationConfig": generation_config
        });

        if !system.trim().is_empty() {
            body["system_instruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        body
    }

    async fn generate_content_call_with_retry(
        &self,
        run_id: Uuid,
        api_key: &str,
        body: serde_json::Value,
        max_attempts: u8,
        attempt_timeout_seconds: u64,
    ) -> Result<serde_json::Value, LlmError> {
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            self.model
        );
        post_json_with_retry(JsonPostRequest {
            client: &self.client,
            run_id,
            provider: "gemini",
            model: &self.model,
            url,
            headers: vec![("x-goog-api-key", api_key.to_string())],
            body,
            extra_no_retry_statuses: &EXTRA_NO_RETRY_STATUSES,
            payload_validation_status: None,
            max_attempts,
            per_attempt_timeout_seconds: attempt_timeout_seconds,
        })
        .await
    }
}

fn extract_candidate_text(payload: &serde_json::Value) -> Option<String> {
    let texts = payload
        .get("candidates")
        .and_then(|v| v.as_array())?
        .iter()
        .filter_map(|candidate| candidate.get("content"))
        .filter_map(|content| content.get("parts"))
        .filter_map(|parts| parts.as_array())
        .flat_map(|parts| parts.iter())
        .filter(|part| {
            !part
                .get("thought")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect::<Vec<_>>();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn extract_usage(payload: &serde_json::Value) -> UsageStats {
    let usage = payload.get("usageMetadata");
    UsageStats {
        estimated_input_tokens: None,
        actual_input_tokens: usage
            .and_then(|u| u.get("promptTokenCount"))
            .and_then(|v| v.as_u64()),
        actual_output_tokens: usage
            .and_then(|u| u.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advise_fallback_without_api_key() {
        let svc = GeminiService::new(
            None,
            "https://generativelanguage.googleapis.com".to_string(),
            "gemini-3.1-pro-preview".to_string(),
            None,
        );
        let out = svc.advise("hello").await.expect("fallback should work");
        assert!(out.contains("local-fallback"));
    }

    #[tokio::test]
    async fn generate_without_key_returns_model_unavailable() {
        let svc = GeminiService::new(
            None,
            "https://generativelanguage.googleapis.com".to_string(),
            "gemini-3.1-pro-preview".to_string(),
            None,
        );
        let err = svc
            .generate_lab_files(&LabGenerationInput {
                run_id: Uuid::nil(),
                mode: "test".to_string(),
                system: "sys".to_string(),
                user_message: "user".to_string(),
            })
            .await
            .expect_err("missing key must fail");
        assert_eq!(err.kind, LlmErrorKind::ModelUnavailable);
    }

    #[test]
    fn extracts_candidate_text() {
        let payload = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "hidden", "thought": true},
                        {"text": "visible"}
                    ]
                }
            }]
        });
        assert_eq!(extract_candidate_text(&payload).as_deref(), Some("visible"));
    }
}
