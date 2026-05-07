use axum::{extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth_creator::extract_auth_context,
    models::{
        api::{ApiResponse, ExecuteResponse, ExecuteStructuredRequest},
        run::RunMode,
    },
    services::{
        qualification::{
            build_qualification_base_lab_block, qualification_allows_generation,
            qualify_lab_request, QualificationInput,
        },
        structured_form::normalize_structured_execution,
    },
    state::AppState,
};

struct PersistedRunInput {
    creator_id: String,
    request_id: Option<Uuid>,
    prompt: String,
    mode: RunMode,
    source_objects: Vec<String>,
    source_objects_for_consume: Option<Vec<String>>,
    has_options: bool,
    summary_reason: &'static str,
}

pub async fn execute_structured_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ExecuteStructuredRequest>,
) -> Result<Json<ApiResponse<ExecuteResponse>>, AppError> {
    let auth = extract_auth_context(&headers, state.config.require_creator_role)?;
    let requested_run_id = parse_requested_run_id(payload.request_id.as_deref())?;
    let has_options = payload.options.is_some();
    let normalized = normalize_structured_execution(payload)?;

    let source_objects =
        resolve_structured_source_objects(&state, &auth.user_id, requested_run_id, normalized.mode)
            .await?;
    let base_lab_block = if source_objects.is_empty() {
        None
    } else {
        let run_id = requested_run_id.expect("variant source resolution requires request_id");
        build_qualification_base_lab_block(&state.storage, &run_id.to_string(), &source_objects)
            .await?
    };

    let qualification_request_id = requested_run_id.unwrap_or_else(Uuid::new_v4);
    let qualification = qualify_lab_request(
        &state.llm,
        QualificationInput {
            request_id: qualification_request_id,
            lab_type: normalized.lab_type,
            lab_request_xml: &normalized.prompt,
            base_lab_block: base_lab_block.as_deref(),
        },
    )
    .await?;

    if !qualification_allows_generation(&qualification) {
        return Err(AppError::BadRequest(format!(
            "lab qualification blocked generation: verdict={}, summary={}",
            qualification.verdict, qualification.resume_utilisateur
        )));
    }

    let source_objects_for_consume = if source_objects.is_empty() {
        None
    } else {
        Some(source_objects.clone())
    };

    let persisted = PersistedRunInput {
        creator_id: auth.user_id,
        request_id: requested_run_id,
        prompt: normalized.prompt,
        mode: normalized.mode,
        source_objects,
        source_objects_for_consume,
        has_options,
        summary_reason: "structured mode accepted",
    };

    let data = enqueue_persisted_run(&state, persisted).await?;

    Ok(Json(ApiResponse::success(data, Uuid::new_v4().to_string())))
}

async fn enqueue_persisted_run(
    state: &AppState,
    input: PersistedRunInput,
) -> Result<ExecuteResponse, AppError> {
    let request_id = input.request_id.unwrap_or_else(Uuid::new_v4);
    state
        .runs_repo
        .create_run(
            request_id,
            &input.creator_id,
            input.prompt,
            Some(input.mode),
            input.source_objects,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                request_id = %request_id,
                error = %error,
                "failed to persist run before enqueue"
            );
            AppError::Internal(
                "une erreur s'est produite veuillez re essayer plus tard".to_string(),
            )
        })?;

    if let Err(error) = state.queue.enqueue_process_run(request_id).await {
        tracing::error!(
            request_id = %request_id,
            error = %error,
            "failed to enqueue run"
        );
        if let Err(mark_error) = state
            .runs_repo
            .mark_failed(request_id, "ENQUEUE_FAILED", &error.to_string())
            .await
        {
            tracing::error!(
                request_id = %request_id,
                error = %mark_error,
                "failed to persist ENQUEUE_FAILED status"
            );
        }
        return Err(AppError::Internal(
            "une erreur s'est produite veuillez re essayer plus tard".to_string(),
        ));
    }

    if let Some(object_keys) = input.source_objects_for_consume {
        if let Err(error) = state
            .run_uploads_repo
            .mark_consumed(request_id, &input.creator_id, &object_keys)
            .await
        {
            tracing::warn!(
                request_id = %request_id,
                creator_id = %input.creator_id,
                error = %error,
                "failed to mark run uploads as consumed"
            );
        }
    }

    let options_note = if input.has_options {
        " with options"
    } else {
        ""
    };
    Ok(ExecuteResponse {
        request_id: request_id.to_string(),
        status: "queued".to_string(),
        status_url: format!("/api/ia/labs/runs/{}", request_id),
        summary: format!("Run accepted ({}){}", input.summary_reason, options_note),
    })
}

fn parse_requested_run_id(raw: Option<&str>) -> Result<Option<Uuid>, AppError> {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    Uuid::parse_str(value)
        .map(Some)
        .map_err(|_| AppError::BadRequest("invalid request_id format".to_string()))
}

async fn resolve_structured_source_objects(
    state: &AppState,
    creator_id: &str,
    requested_run_id: Option<Uuid>,
    mode: RunMode,
) -> Result<Vec<String>, AppError> {
    if matches!(mode, RunMode::GenerateFromScratch) {
        return Ok(Vec::new());
    }

    let run_id = requested_run_id.ok_or_else(|| {
        AppError::BadRequest("request_id is required for create_variant mode".to_string())
    })?;

    let registered = state
        .run_uploads_repo
        .list_uploaded_object_keys(run_id, creator_id)
        .await
        .map_err(|error| {
            tracing::error!(
                request_id = %run_id,
                creator_id = %creator_id,
                error = %error,
                "failed to load registered run uploads"
            );
            AppError::Internal(
                "une erreur s'est produite veuillez re essayer plus tard".to_string(),
            )
        })?;

    if registered.is_empty() {
        return Err(AppError::BadRequest(
            "create_variant requires at least one uploaded source file".to_string(),
        ));
    }

    Ok(registered)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    #[test]
    fn parse_requested_run_id_rejects_invalid_uuid() {
        let err = super::parse_requested_run_id(Some("bad-uuid")).expect_err("must fail");
        assert!(err.to_string().contains("invalid request_id format"));
    }

    #[test]
    fn parse_requested_run_id_accepts_uuid() {
        let id = Uuid::new_v4();
        let out = super::parse_requested_run_id(Some(&id.to_string())).expect("must parse");
        assert_eq!(out, Some(id));
    }
}
