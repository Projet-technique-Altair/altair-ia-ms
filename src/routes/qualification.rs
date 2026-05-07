use axum::{extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth_creator::extract_auth_context,
    models::{
        api::{ApiResponse, ExecuteStructuredRequest, QualificationResponse},
        run::RunMode,
    },
    services::{
        qualification::{
            build_qualification_base_lab_block, qualify_lab_request, QualificationInput,
        },
        structured_form::normalize_structured_execution,
    },
    state::AppState,
};

pub async fn qualify_structured_lab(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ExecuteStructuredRequest>,
) -> Result<Json<ApiResponse<QualificationResponse>>, AppError> {
    let auth = extract_auth_context(&headers, state.config.require_creator_role)?;
    let requested_run_id = parse_requested_run_id(payload.request_id.as_deref())?;
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

    let request_id = requested_run_id.unwrap_or_else(Uuid::new_v4);
    let qualification = qualify_lab_request(
        &state.llm,
        QualificationInput {
            request_id,
            lab_type: normalized.lab_type,
            lab_request_xml: &normalized.prompt,
            base_lab_block: base_lab_block.as_deref(),
        },
    )
    .await?;

    Ok(Json(ApiResponse::success(
        qualification,
        Uuid::new_v4().to_string(),
    )))
}

pub(crate) fn parse_requested_run_id(raw: Option<&str>) -> Result<Option<Uuid>, AppError> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    Uuid::parse_str(value)
        .map(Some)
        .map_err(|_| AppError::BadRequest("invalid request_id format".to_string()))
}

pub(crate) async fn resolve_structured_source_objects(
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
    use super::parse_requested_run_id;
    use uuid::Uuid;

    #[test]
    fn parse_requested_run_id_rejects_invalid_uuid() {
        let err = parse_requested_run_id(Some("bad-uuid")).expect_err("must fail");
        assert!(err.to_string().contains("invalid request_id format"));
    }

    #[test]
    fn parse_requested_run_id_accepts_uuid() {
        let id = Uuid::new_v4();
        let out = parse_requested_run_id(Some(&id.to_string())).expect("must parse");
        assert_eq!(out, Some(id));
    }
}
