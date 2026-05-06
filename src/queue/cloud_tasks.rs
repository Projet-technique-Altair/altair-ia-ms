use std::{sync::Arc, time::Duration};

use anyhow::Context;
use base64::Engine;
use chrono::Utc;
use gcp_auth::{provider, TokenProvider};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone)]
pub struct CloudTasksConfig {
    pub project_id: String,
    pub location: String,
    pub queue: String,
    pub max_enqueue_attempts: u32,
    pub worker_target_base_url: String,
    pub worker_oidc_service_account: String,
    pub worker_oidc_audience: Option<String>,
}

#[derive(Clone)]
pub struct CloudTasksClient {
    http_client: reqwest::Client,
    token_provider: Arc<dyn TokenProvider>,
    cfg: CloudTasksConfig,
}

#[derive(Serialize)]
struct CreateTaskRequest {
    task: TaskPayload,
}

#[derive(Serialize)]
struct TaskPayload {
    #[serde(rename = "httpRequest")]
    http_request: HttpRequestPayload,
    #[serde(rename = "scheduleTime", skip_serializing_if = "Option::is_none")]
    schedule_time: Option<ScheduleTimePayload>,
}

#[derive(Serialize)]
struct HttpRequestPayload {
    #[serde(rename = "httpMethod")]
    http_method: String,
    url: String,
    headers: std::collections::HashMap<String, String>,
    body: String,
    #[serde(rename = "oidcToken")]
    oidc_token: OidcTokenPayload,
}

#[derive(Serialize)]
struct OidcTokenPayload {
    #[serde(rename = "serviceAccountEmail")]
    service_account_email: String,
    audience: String,
}

#[derive(Clone, Serialize)]
struct ScheduleTimePayload {
    seconds: String,
}

impl CloudTasksClient {
    pub async fn new(cfg: CloudTasksConfig) -> anyhow::Result<Self> {
        let token_provider = provider()
            .await
            .context("failed to init gcp_auth provider")?;

        Ok(Self {
            http_client: reqwest::Client::new(),
            token_provider,
            cfg,
        })
    }

    pub async fn enqueue_run(&self, request_id: Uuid, delay_seconds: u64) -> anyhow::Result<()> {
        let token = self
            .token_provider
            .token(&["https://www.googleapis.com/auth/cloud-platform"])
            .await
            .context("failed to obtain cloud-platform token")?;

        let queue_url = format!(
            "https://cloudtasks.googleapis.com/v2/projects/{}/locations/{}/queues/{}/tasks",
            self.cfg.project_id, self.cfg.location, self.cfg.queue
        );

        let target_url = format!(
            "{}/internal/ia/runs/{}/process",
            self.cfg.worker_target_base_url.trim_end_matches('/'),
            request_id
        );

        let audience = self
            .cfg
            .worker_oidc_audience
            .clone()
            .unwrap_or_else(|| self.cfg.worker_target_base_url.clone());
        let schedule_time = if delay_seconds == 0 {
            None
        } else {
            Some(ScheduleTimePayload {
                seconds: (Utc::now().timestamp() + delay_seconds as i64).to_string(),
            })
        };

        let max_attempts = self.cfg.max_enqueue_attempts.clamp(1, 10);
        for attempt in 1..=max_attempts {
            let body = CreateTaskRequest {
                task: TaskPayload {
                    http_request: HttpRequestPayload {
                        http_method: "POST".to_string(),
                        url: target_url.clone(),
                        headers: std::collections::HashMap::from([(
                            "Content-Type".to_string(),
                            "application/json".to_string(),
                        )]),
                        body: base64::engine::general_purpose::STANDARD
                            .encode(r#"{"source":"cloud_tasks"}"#),
                        oidc_token: OidcTokenPayload {
                            service_account_email: self.cfg.worker_oidc_service_account.clone(),
                            audience: audience.clone(),
                        },
                    },
                    schedule_time: schedule_time.clone(),
                },
            };

            let response = self
                .http_client
                .post(&queue_url)
                .bearer_auth(token.as_str())
                .json(&body)
                .send()
                .await;

            let response = match response {
                Ok(r) => r,
                Err(error) => {
                    if attempt < max_attempts {
                        let delay_ms = 200 * attempt as u64;
                        tracing::warn!(
                            attempt,
                            max_attempts,
                            delay_ms,
                            error = %error,
                            "Cloud Tasks enqueue transport error, retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    anyhow::bail!("failed to call Cloud Tasks API: {error}");
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(());
            }

            let txt = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable body>".to_string());
            let retryable =
                status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            if retryable && attempt < max_attempts {
                let delay_ms = 200 * attempt as u64;
                tracing::warn!(
                    attempt,
                    max_attempts,
                    delay_ms,
                    status = %status,
                    body = %txt,
                    "Cloud Tasks enqueue HTTP retryable error, retrying"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                continue;
            }
            anyhow::bail!("Cloud Tasks enqueue failed: HTTP {} - {}", status, txt);
        }

        anyhow::bail!("Cloud Tasks enqueue failed after retries");
    }
}
