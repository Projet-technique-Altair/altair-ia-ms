use axum::{extract::Path, extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{error::AppError, models::api::ApiResponse, state::AppState};

async fn verify_internal_request(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if internal_worker_token_matches(state.config.internal_worker_token.as_deref(), headers) {
        return Ok(());
    }

    if let Some(verifier) = &state.oidc_verifier {
        return verifier.verify_headers(headers).await;
    }

    if state.config.internal_worker_token.is_some() {
        return Err(AppError::Unauthorized(
            "invalid internal worker token".to_string(),
        ));
    }

    Err(AppError::Unauthorized(
        "internal endpoint auth is not configured".to_string(),
    ))
}

fn internal_worker_token_matches(configured_token: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(token) = configured_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };

    headers
        .get("x-internal-worker-token")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|incoming| incoming == token)
}

pub async fn process_run_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    verify_internal_request(&state, &headers).await?;

    let request_id =
        Uuid::parse_str(&run_id).map_err(|_| AppError::BadRequest("invalid run id".to_string()))?;

    state.run_processor.process_run(request_id).await;

    Ok(Json(ApiResponse::success(
        serde_json::json!({"request_id": request_id, "status": "processed"}),
        Uuid::new_v4().to_string(),
    )))
}

pub async fn pedagogical_analysis_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    verify_internal_request(&state, &headers).await?;

    let report_type = payload
        .get("report_type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AppError::BadRequest("report_type is required".to_string()))?;

    if !matches!(
        report_type,
        "individual_student_activity_report" | "group_activity_report"
    ) {
        return Err(AppError::BadRequest(
            "unsupported pedagogical analysis report_type".to_string(),
        ));
    }

    let prompt = build_pedagogical_prompt(&payload);
    let request_id = extract_payload_request_id(&payload).unwrap_or_else(Uuid::new_v4);
    let raw = state
        .llm
        .advise_with_context(&prompt, "report", request_id)
        .await
        .map_err(|e| {
            AppError::AiTemporarilyUnavailable(format!("pedagogical analysis failed: {e}"))
        })?;

    if raw.starts_with("(local-fallback)") {
        return Ok(Json(ApiResponse::success(
            local_structured_report(&payload),
            Uuid::new_v4().to_string(),
        )));
    }

    match parse_and_validate_report(&raw, report_type) {
        Ok(report) => Ok(Json(ApiResponse::success(
            report,
            Uuid::new_v4().to_string(),
        ))),
        Err(validation_error) => {
            let retry_prompt = build_retry_prompt(&payload, &validation_error, &raw);
            let retry_raw = state
                .llm
                .advise_with_context(&retry_prompt, "report", request_id)
                .await
                .map_err(|e| {
                    AppError::AiTemporarilyUnavailable(format!(
                        "pedagogical analysis retry failed: {e}"
                    ))
                })?;

            let report = parse_and_validate_report(&retry_raw, report_type).map_err(|e| {
                AppError::Internal(format!(
                    "pedagogical analysis response did not match schema: {e}"
                ))
            })?;

            Ok(Json(ApiResponse::success(
                report,
                Uuid::new_v4().to_string(),
            )))
        }
    }
}

fn extract_payload_request_id(payload: &serde_json::Value) -> Option<Uuid> {
    payload
        .get("request_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn build_pedagogical_prompt(payload: &serde_json::Value) -> String {
    format!(
        r#"You are a pedagogical assistant for teachers and terminal lab creators.

You must produce only valid JSON. No markdown, no surrounding text.

Absolute constraints:
- never grade a student;
- never officially decide success or failure;
- never rank students;
- never accuse cheating;
- never invent facts absent from the payload;
- distinguish observed facts from pedagogical hypotheses.

Expected schema for an individual report:
{{
  "report_type": "individual_student_activity_report",
  "summary": "",
  "observed_activity": {{
    "started_lab": true,
    "completed_lab": false,
    "terminal_interactions": "low|medium|high|unknown",
    "answers_submitted": 0
  }},
  "effort_analysis": {{
    "level": "low|medium|high|unknown",
    "evidence": []
  }},
  "progression_analysis": {{
    "progression_level": "none|partial|significant|completed|unknown",
    "evidence": [],
    "blockers": []
  }},
  "recommendations_for_teacher": []
}}

Expected schema for a group report:
{{
  "report_type": "group_activity_report",
  "summary": "",
  "group_observations": [],
  "difficulty_signals": [],
  "possible_lab_improvements": [],
  "recommendations_for_teacher": []
}}

Factual payload:
{}"#,
        payload
    )
}

fn build_retry_prompt(payload: &serde_json::Value, validation_error: &str, raw: &str) -> String {
    format!(
        r#"The previous output is invalid.

Validation error: {validation_error}

Fix only the JSON format while following the expected schema. Do not change the provided facts.
Respond only with valid JSON.

Previous output:
{raw}

Factual payload:
{payload}"#
    )
}

fn parse_and_validate_report(
    raw: &str,
    expected_report_type: &str,
) -> Result<serde_json::Value, String> {
    let json_text = extract_json_object(raw).ok_or_else(|| "missing JSON object".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| format!("invalid JSON: {e}"))?;

    let report_type = value
        .get("report_type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "report_type missing".to_string())?;

    if report_type != expected_report_type {
        return Err(format!(
            "report_type mismatch: expected {expected_report_type}, got {report_type}"
        ));
    }

    if value
        .get("summary")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err("summary missing".to_string());
    }

    if value
        .get("recommendations_for_teacher")
        .and_then(|value| value.as_array())
        .is_none()
    {
        return Err("recommendations_for_teacher must be an array".to_string());
    }

    if expected_report_type == "individual_student_activity_report" {
        for key in [
            "observed_activity",
            "effort_analysis",
            "progression_analysis",
        ] {
            if !value.get(key).is_some_and(|value| value.is_object()) {
                return Err(format!("{key} must be an object"));
            }
        }
    }

    Ok(value)
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if start > end {
        return None;
    }
    Some(&raw[start..=end])
}

fn local_structured_report(payload: &serde_json::Value) -> serde_json::Value {
    let report_type = payload
        .get("report_type")
        .and_then(|value| value.as_str())
        .unwrap_or("individual_student_activity_report");

    if report_type == "group_activity_report" {
        let metrics = payload.get("group_activity").cloned().unwrap_or_default();
        return serde_json::json!({
            "report_type": "group_activity_report",
            "summary": "Local summary based on the available backend metrics.",
            "group_observations": [
                format!("Observed students: {}", metrics.get("students_count").and_then(|v| v.as_i64()).unwrap_or(0)),
                format!("Sessions with terminal activity: {}", metrics.get("terminal_sessions_count").and_then(|v| v.as_i64()).unwrap_or(0))
            ],
            "difficulty_signals": metrics.get("common_blockers").cloned().unwrap_or_else(|| serde_json::json!([])),
            "possible_lab_improvements": [
                "Check the steps that concentrate failures or repeated attempts."
            ],
            "recommendations_for_teacher": [
                "Use factual metrics to target classroom support."
            ]
        });
    }

    let metrics = payload.get("student_activity").cloned().unwrap_or_default();
    let commands_count = metrics
        .get("commands_count")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let level = if commands_count >= 30 {
        "high"
    } else if commands_count >= 8 {
        "medium"
    } else if commands_count > 0 {
        "low"
    } else {
        "unknown"
    };

    serde_json::json!({
        "report_type": "individual_student_activity_report",
        "summary": "Local summary based on the available backend metrics.",
        "observed_activity": {
            "started_lab": metrics.get("started_lab").and_then(|v| v.as_bool()).unwrap_or(false),
            "completed_lab": metrics.get("completed_lab").and_then(|v| v.as_bool()).unwrap_or(false),
            "terminal_interactions": level,
            "answers_submitted": metrics.get("validations_succeeded").and_then(|v| v.as_i64()).unwrap_or(0)
                + metrics.get("validations_failed").and_then(|v| v.as_i64()).unwrap_or(0)
        },
        "effort_analysis": {
            "level": level,
            "evidence": [
                format!("Collected command count: {commands_count}")
            ]
        },
        "progression_analysis": {
            "progression_level": if metrics.get("completed_lab").and_then(|v| v.as_bool()).unwrap_or(false) { "completed" } else { "partial" },
            "evidence": metrics.get("completed_steps").cloned().unwrap_or_else(|| serde_json::json!([])),
            "blockers": metrics.get("possible_blockers").cloned().unwrap_or_else(|| serde_json::json!([]))
        },
        "recommendations_for_teacher": [
            "Use significant commands and likely blockers before intervening."
        ]
    })
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::internal_worker_token_matches;

    #[test]
    fn internal_worker_token_match_accepts_configured_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-worker-token",
            HeaderValue::from_static("shared-token"),
        );

        assert!(internal_worker_token_matches(
            Some("shared-token"),
            &headers
        ));
    }

    #[test]
    fn internal_worker_token_match_rejects_missing_or_wrong_header() {
        let mut headers = HeaderMap::new();
        assert!(!internal_worker_token_matches(
            Some("shared-token"),
            &headers
        ));

        headers.insert("x-internal-worker-token", HeaderValue::from_static("wrong"));
        assert!(!internal_worker_token_matches(
            Some("shared-token"),
            &headers
        ));
        assert!(!internal_worker_token_matches(None, &headers));
        assert!(!internal_worker_token_matches(Some(" "), &headers));
    }
}
