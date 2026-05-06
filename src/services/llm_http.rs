use std::time::{Duration, Instant};

use reqwest::{header::RETRY_AFTER, StatusCode};
use serde_json::Value;
use uuid::Uuid;

use crate::services::{
    llm::{truncate_for_log, LlmError, LlmErrorKind},
    llm_config::{
        MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS, NO_RETRY_ON_HTTP_STATUS, RETRY_BACKOFF_SECONDS,
        RETRY_ON_HTTP_STATUS, TOTAL_TIMEOUT_SECONDS,
    },
};

pub(crate) struct JsonPostRequest<'a> {
    pub client: &'a reqwest::Client,
    pub run_id: Uuid,
    pub provider: &'static str,
    pub model: &'a str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
    pub extra_no_retry_statuses: &'static [u16],
    pub payload_validation_status: Option<u16>,
    pub max_attempts: u8,
    pub per_attempt_timeout_seconds: u64,
}

pub(crate) async fn post_json_with_retry(request: JsonPostRequest<'_>) -> Result<Value, LlmError> {
    let started = Instant::now();
    let max_attempts = request.max_attempts.max(1);
    let per_attempt_timeout_seconds = request.per_attempt_timeout_seconds.max(1);

    for attempt in 1..=max_attempts {
        if started.elapsed() > Duration::from_secs(TOTAL_TIMEOUT_SECONDS) {
            return Err(LlmError::new(
                LlmErrorKind::ModelUnavailable,
                format!(
                    "total timeout reached ({}s) before attempt {}",
                    TOTAL_TIMEOUT_SECONDS, attempt
                ),
            )
            .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS)));
        }

        tracing::info!(
            run_id = %request.run_id,
            stage = "api_call",
            provider = request.provider,
            model = %request.model,
            attempt,
            "calling llm endpoint"
        );

        let mut builder = request.client.post(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }
        let send_future = builder.json(&request.body).send();

        let response = match tokio::time::timeout(
            Duration::from_secs(per_attempt_timeout_seconds),
            send_future,
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(error)) => {
                if attempt < max_attempts {
                    let delay = backoff_for_attempt(attempt);
                    tracing::warn!(
                        run_id = %request.run_id,
                        stage = "api_call",
                        provider = request.provider,
                        attempt,
                        delay_seconds = delay,
                        error = %error,
                        "llm transport error, retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
                return Err(LlmError::new(
                    LlmErrorKind::ModelUnavailable,
                    format!(
                        "{} transport error after retries: {error}",
                        request.provider
                    ),
                )
                .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS)));
            }
            Err(_) => {
                if attempt < max_attempts {
                    let delay = backoff_for_attempt(attempt);
                    tracing::warn!(
                        run_id = %request.run_id,
                        stage = "api_call",
                        provider = request.provider,
                        attempt,
                        delay_seconds = delay,
                        timeout_seconds = per_attempt_timeout_seconds,
                        "llm per-attempt timeout, retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
                return Err(LlmError::new(
                    LlmErrorKind::ModelUnavailable,
                    format!("{} request timeout after retries", request.provider),
                )
                .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS)));
            }
        };

        let status = response.status();
        let retry_after = parse_retry_after_seconds(response.headers().get(RETRY_AFTER));
        let text_body = response.text().await.unwrap_or_default();

        if status.is_success() {
            let payload: Value = serde_json::from_str(&text_body).map_err(|e| {
                LlmError::new(
                    LlmErrorKind::Decode,
                    format!("failed to decode {} JSON payload: {e}", request.provider),
                )
                .with_status(status)
            })?;
            return Ok(payload);
        }

        let status_u16 = status.as_u16();
        let is_no_retry = NO_RETRY_ON_HTTP_STATUS.contains(&status_u16)
            || request.extra_no_retry_statuses.contains(&status_u16);
        let is_retryable = RETRY_ON_HTTP_STATUS.contains(&status_u16) || status.is_server_error();

        if request.payload_validation_status == Some(status_u16) {
            tracing::error!(
                run_id = %request.run_id,
                stage = "api_call",
                provider = request.provider,
                status = status_u16,
                payload_bytes = request.body.to_string().len(),
                response_summary = %provider_error_summary(&text_body),
                "llm payload validation error"
            );
        }

        if is_no_retry {
            return Err(LlmError::new(
                map_status_to_error_kind(status),
                format!(
                    "{} non-retryable HTTP {}: {}",
                    request.provider,
                    status_u16,
                    provider_error_summary(&text_body)
                ),
            )
            .with_status(status));
        }

        if is_retryable && attempt < max_attempts {
            let delay = retry_after.unwrap_or_else(|| backoff_for_attempt(attempt));
            tracing::warn!(
                run_id = %request.run_id,
                stage = "api_call",
                provider = request.provider,
                attempt,
                status = status_u16,
                delay_seconds = delay,
                "llm retryable status, retrying"
            );
            tokio::time::sleep(Duration::from_secs(delay)).await;
            continue;
        }

        return Err(LlmError::new(
            LlmErrorKind::ModelUnavailable,
            format!(
                "{} unavailable after {} attempts (last HTTP {}): {}",
                request.provider,
                max_attempts,
                status_u16,
                provider_error_summary(&text_body)
            ),
        )
        .with_status(status)
        .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS)));
    }

    Err(LlmError::new(
        LlmErrorKind::ModelUnavailable,
        format!(
            "{} unavailable after {} attempts",
            request.provider, max_attempts
        ),
    )
    .with_retry_after(Some(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS)))
}

fn provider_error_summary(text: &str) -> String {
    if let Ok(payload) = serde_json::from_str::<Value>(text) {
        if let Some(error) = payload.get("error") {
            let status = error
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let code = error
                .get("code")
                .and_then(|v| v.as_i64())
                .map(|v| v.to_string())
                .unwrap_or_default();

            let summary = [status, message, code.as_str()]
                .into_iter()
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" | ");

            if !summary.is_empty() {
                return truncate_for_log(&summary, 1_000);
            }
        }
    }

    truncate_for_log(text, 1_000)
}

fn parse_retry_after_seconds(header: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    header
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

fn backoff_for_attempt(attempt: u8) -> u64 {
    let idx = attempt.saturating_sub(1) as usize;
    RETRY_BACKOFF_SECONDS
        .get(idx)
        .copied()
        .unwrap_or_else(|| *RETRY_BACKOFF_SECONDS.last().unwrap_or(&8))
}

pub(crate) fn map_status_to_error_kind(status: StatusCode) -> LlmErrorKind {
    match status {
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => LlmErrorKind::InvalidRequest,
        StatusCode::UNAUTHORIZED => LlmErrorKind::Unauthorized,
        StatusCode::FORBIDDEN => LlmErrorKind::Forbidden,
        StatusCode::UNPROCESSABLE_ENTITY => LlmErrorKind::Unprocessable,
        StatusCode::TOO_MANY_REQUESTS => LlmErrorKind::RateLimited,
        _ if status.is_server_error() => LlmErrorKind::ServerError,
        _ => LlmErrorKind::Transport,
    }
}
