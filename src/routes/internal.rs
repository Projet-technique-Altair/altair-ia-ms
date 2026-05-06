use axum::{extract::Path, extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{error::AppError, models::api::ApiResponse, state::AppState};

async fn verify_internal_request(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if let Some(verifier) = &state.oidc_verifier {
        verifier.verify_headers(headers).await?;
    } else if let Some(token) = &state.config.internal_worker_token {
        let incoming = headers
            .get("x-internal-worker-token")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default();

        if incoming != token {
            return Err(AppError::Unauthorized(
                "invalid internal worker token".to_string(),
            ));
        }
    } else {
        return Err(AppError::Unauthorized(
            "internal endpoint auth is not configured".to_string(),
        ));
    }

    Ok(())
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
        r#"Tu es un assistant pedagogique pour des professeurs et createurs de labs terminal.

Tu dois produire uniquement un JSON valide. Aucun markdown, aucun texte autour.

Contraintes absolues :
- ne note jamais un eleve ;
- ne decide jamais officiellement de la reussite ou de l'echec ;
- ne classe jamais les eleves ;
- n'accuse jamais de triche ;
- n'invente aucun fait absent du payload ;
- distingue les faits observes des hypotheses pedagogiques.

Schema attendu pour un rapport individuel :
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

Schema attendu pour un rapport groupe :
{{
  "report_type": "group_activity_report",
  "summary": "",
  "group_observations": [],
  "difficulty_signals": [],
  "possible_lab_improvements": [],
  "recommendations_for_teacher": []
}}

Payload factuel :
{}"#,
        payload
    )
}

fn build_retry_prompt(payload: &serde_json::Value, validation_error: &str, raw: &str) -> String {
    format!(
        r#"La sortie precedente est invalide.

Erreur de validation : {validation_error}

Corrige uniquement le format JSON en respectant le schema attendu. Ne change pas les faits fournis.
Reponds uniquement avec un JSON valide.

Sortie precedente :
{raw}

Payload factuel :
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
            "summary": "Synthese locale basee sur les metriques backend disponibles.",
            "group_observations": [
                format!("Etudiants observes: {}", metrics.get("students_count").and_then(|v| v.as_i64()).unwrap_or(0)),
                format!("Sessions avec terminal: {}", metrics.get("terminal_sessions_count").and_then(|v| v.as_i64()).unwrap_or(0))
            ],
            "difficulty_signals": metrics.get("common_blockers").cloned().unwrap_or_else(|| serde_json::json!([])),
            "possible_lab_improvements": [
                "Verifier les etapes qui concentrent les echecs ou les repetitions."
            ],
            "recommendations_for_teacher": [
                "Utiliser les metriques factuelles pour cibler l'accompagnement en classe."
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
        "summary": "Synthese locale basee sur les metriques backend disponibles.",
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
                format!("Nombre de commandes collectees: {commands_count}")
            ]
        },
        "progression_analysis": {
            "progression_level": if metrics.get("completed_lab").and_then(|v| v.as_bool()).unwrap_or(false) { "completed" } else { "partial" },
            "evidence": metrics.get("completed_steps").cloned().unwrap_or_else(|| serde_json::json!([])),
            "blockers": metrics.get("possible_blockers").cloned().unwrap_or_else(|| serde_json::json!([]))
        },
        "recommendations_for_teacher": [
            "S'appuyer sur les commandes significatives et les blocages probables avant d'intervenir."
        ]
    })
}
