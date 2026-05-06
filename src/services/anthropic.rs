use serde_json::json;
use uuid::Uuid;

use crate::services::{
    llm::{
        estimate_tokens, local_advise_fallback, truncate_for_log, LabGenerationInput,
        LabGenerationOutput, LlmError, LlmErrorKind, TokenCountEstimate, UsageStats,
        NO_RESPONSE_TEXT,
    },
    llm_config::{
        MAX_OUTPUT_TOKENS, MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS, PER_ATTEMPT_TIMEOUT_SECONDS,
        RETRY_MAX_ATTEMPTS,
    },
    llm_http::{map_status_to_error_kind, post_json_with_retry, JsonPostRequest},
};

#[derive(Clone)]
pub struct AnthropicService {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
}

impl AnthropicService {
    pub fn new(api_key: Option<String>, base_url: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
            model,
        }
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    pub async fn count_tokens(
        &self,
        run_id: Uuid,
        system: &str,
        user_message: &str,
    ) -> Result<TokenCountEstimate, LlmError> {
        if self.api_key.is_none() {
            let estimated = estimate_tokens(system) + estimate_tokens(user_message);
            return Ok(TokenCountEstimate {
                input_tokens: estimated,
                from_api: false,
            });
        }

        let key = self.api_key.clone().unwrap_or_default();
        let url = format!(
            "{}/v1/messages/count_tokens",
            self.base_url.trim_end_matches('/')
        );
        let body = json!({
            "model": self.model,
            "system": system,
            "messages": [{"role": "user", "content": user_message}]
        });

        let response = self
            .client
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                LlmError::new(
                    LlmErrorKind::Transport,
                    format!("count_tokens transport error: {e}"),
                )
            })?;

        let status = response.status();
        let payload: serde_json::Value = response.json().await.map_err(|e| {
            LlmError::new(
                LlmErrorKind::Decode,
                format!("count_tokens decode error: {e}"),
            )
        })?;

        if !status.is_success() {
            return Err(LlmError::new(
                map_status_to_error_kind(status),
                format!(
                    "count_tokens failed with HTTP {} payload={}",
                    status,
                    truncate_for_log(&payload.to_string(), 4000)
                ),
            )
            .with_status(status));
        }

        let input_tokens = payload
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::Decode,
                    "count_tokens response missing input_tokens",
                )
            })?;

        tracing::info!(
            run_id = %run_id,
            stage = "pre_dispatch",
            model = %self.model,
            estimated_input_tokens = input_tokens,
            "count_tokens completed"
        );

        Ok(TokenCountEstimate {
            input_tokens,
            from_api: true,
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
                "ANTHROPIC_API_KEY is not configured",
            )
            .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS))
        })?;

        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": input.system,
            "messages": [{"role": "user", "content": input.user_message}]
        });

        let payload = self
            .messages_call_with_retry(
                input.run_id,
                &key,
                body,
                max_attempts,
                attempt_timeout_seconds,
            )
            .await?;
        let answer = extract_first_text_block(&payload)
            .ok_or_else(|| LlmError::new(LlmErrorKind::EmptyResponse, NO_RESPONSE_TEXT))?;

        let usage = UsageStats {
            estimated_input_tokens: None,
            actual_input_tokens: payload
                .get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64()),
            actual_output_tokens: payload
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64()),
        };

        Ok(LabGenerationOutput {
            raw_response: answer,
            usage,
            provider: "anthropic".to_string(),
            model: self.model.clone(),
            used_fallback: false,
        })
    }

    pub async fn advise(&self, prompt: &str) -> anyhow::Result<String> {
        if self.api_key.is_none() {
            return Ok(local_advise_fallback(prompt));
        }

        let key = self.api_key.clone().unwrap_or_default();
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "messages": [{"role": "user", "content": prompt}]
        });

        let payload = self
            .messages_call_with_retry(
                Uuid::nil(),
                &key,
                body,
                RETRY_MAX_ATTEMPTS,
                PER_ATTEMPT_TIMEOUT_SECONDS,
            )
            .await
            .map_err(anyhow::Error::msg)?;

        let answer =
            extract_first_text_block(&payload).unwrap_or_else(|| NO_RESPONSE_TEXT.to_string());

        Ok(answer)
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
                "ANTHROPIC_API_KEY is not configured",
            )
            .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS))
        })?;

        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "messages": [{"role": "user", "content": prompt}]
        });

        let payload = self
            .messages_call_with_retry(request_id, &key, body, 1, attempt_timeout_seconds)
            .await?;

        Ok(extract_first_text_block(&payload).unwrap_or_else(|| NO_RESPONSE_TEXT.to_string()))
    }

    async fn messages_call_with_retry(
        &self,
        run_id: Uuid,
        api_key: &str,
        body: serde_json::Value,
        max_attempts: u8,
        attempt_timeout_seconds: u64,
    ) -> Result<serde_json::Value, LlmError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        post_json_with_retry(JsonPostRequest {
            client: &self.client,
            run_id,
            provider: "anthropic",
            model: &self.model,
            url,
            headers: vec![
                ("x-api-key", api_key.to_string()),
                ("anthropic-version", "2023-06-01".to_string()),
            ],
            body,
            extra_no_retry_statuses: &[],
            payload_validation_status: Some(422),
            max_attempts,
            per_attempt_timeout_seconds: attempt_timeout_seconds,
        })
        .await
    }
}

fn extract_first_text_block(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            let texts = arr
                .iter()
                .filter_map(|node| {
                    let node_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                    if node_type == "text" {
                        node.get("text")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advise_fallback_without_api_key() {
        let svc = AnthropicService::new(
            None,
            "https://api.anthropic.com".to_string(),
            "claude-sonnet-4-6".to_string(),
        );
        let out = svc.advise("hello").await.expect("fallback should work");
        assert!(out.contains("local-fallback"));
    }

    #[tokio::test]
    async fn generate_without_key_returns_model_unavailable() {
        let svc = AnthropicService::new(
            None,
            "https://api.anthropic.com".to_string(),
            "claude-sonnet-4-6".to_string(),
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
}
