use axum::{extract::Path, extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth_creator::extract_auth_context,
    models::api::{ApiResponse, PresignDownloadResponse, RunStatusResponse},
    state::AppState,
};

fn is_run_failure_retryable(error_code: Option<&str>) -> bool {
    matches!(
        error_code,
        Some("ENQUEUE_FAILED")
            | Some("SOURCE_DOWNLOAD_FAILED")
            | Some("RESULT_UPLOAD_FAILED")
            | Some("TIMEOUT")
            | Some("TOOLCHAIN_FAILED")
            | Some("AI_TEMPORARILY_UNAVAILABLE")
            | Some("MODEL_UNAVAILABLE_RETRY")
    )
}

const GENERIC_RUN_ERROR_MESSAGE: &str = "Une erreur est survenue.";

fn public_error_message(error_code: Option<&str>) -> Option<String> {
    let code = error_code?;
    let message = match code {
        "ENQUEUE_FAILED" => "Une erreur s'est produite, veuillez reessayer plus tard.",
        "SOURCE_DOWNLOAD_FAILED" => "Impossible de recuperer les fichiers source. Veuillez reessayer.",
        "SOURCE_PARSE_FAILED" => {
            "Les fichiers fournis sont invalides ou illisibles. Verifiez leur format puis reessayez."
        }
        "UNSATISFIABLE_REQUEST" => {
            "La demande ne peut pas etre satisfaite avec les contraintes fournies."
        }
        "MODE_APPLICATION_FAILED"
        | "RESULT_BUILD_FAILED"
        | "OUTPUT_PARSE_FAILED"
        | "MODEL_CALL_FAILED"
        | "PRE_DISPATCH_FAILED"
        | "PROMPT_BUILD_FAILED"
        | "MODEL_AUTH_FAILED" => {
            GENERIC_RUN_ERROR_MESSAGE
        }
        "PROMPT_TOO_LARGE" => {
            "La demande est trop volumineuse pour etre traitee. Reduisez le contexte puis reessayez."
        }
        "VALIDATION_FAILED" => GENERIC_RUN_ERROR_MESSAGE,
        "AI_TEMPORARILY_UNAVAILABLE" => {
            "Le service IA est temporairement indisponible. Veuillez reessayer plus tard."
        }
        "MODEL_UNAVAILABLE" | "MODEL_UNAVAILABLE_RETRY" => GENERIC_RUN_ERROR_MESSAGE,
        "RESULT_UPLOAD_FAILED" => {
            "Le resultat n'a pas pu etre enregistre. Veuillez reessayer."
        }
        "TIMEOUT" => GENERIC_RUN_ERROR_MESSAGE,
        "RUN_NOT_FOUND" => "Run introuvable.",
        _ => GENERIC_RUN_ERROR_MESSAGE,
    };
    Some(message.to_string())
}

pub async fn get_run_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<RunStatusResponse>>, AppError> {
    let auth = extract_auth_context(&headers, state.config.require_creator_role)?;
    let request_id =
        Uuid::parse_str(&run_id).map_err(|_| AppError::BadRequest("invalid run id".to_string()))?;

    let run = state
        .runs_repo
        .get_run(request_id)
        .await
        .map_err(|error| {
            tracing::error!(
                request_id = %request_id,
                error = %error,
                "failed to fetch run status"
            );
            AppError::Internal(
                "une erreur s'est produite veuillez re essayer plus tard".to_string(),
            )
        })?
        .ok_or_else(|| AppError::NotFound("run not found".to_string()))?;

    if run.creator_id != auth.user_id {
        return Err(AppError::Forbidden(
            "run does not belong to caller".to_string(),
        ));
    }

    let error_code = run.error_code.clone();
    let error_message = public_error_message(error_code.as_deref());

    let data = RunStatusResponse {
        request_id: run.request_id.to_string(),
        status: run.status.as_str().to_string(),
        mode_selected: run.mode_selected,
        retryable: is_run_failure_retryable(error_code.as_deref()),
        used_model_fallback: run.used_model_fallback,
        error_code,
        error_message,
        estimated_input_tokens: run.estimated_input_tokens,
        actual_input_tokens: run.actual_input_tokens,
        actual_output_tokens: run.actual_output_tokens,
        excluded_source_objects: run.excluded_source_objects,
        created_at: run.created_at.to_rfc3339(),
        updated_at: run.updated_at.to_rfc3339(),
        finished_at: run.finished_at.map(|d| d.to_rfc3339()),
    };

    Ok(Json(ApiResponse::success(data, Uuid::new_v4().to_string())))
}

pub async fn presign_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<PresignDownloadResponse>>, AppError> {
    let auth = extract_auth_context(&headers, state.config.require_creator_role)?;
    let request_id =
        Uuid::parse_str(&run_id).map_err(|_| AppError::BadRequest("invalid run id".to_string()))?;

    let run = state
        .runs_repo
        .get_run(request_id)
        .await
        .map_err(|error| {
            tracing::error!(
                request_id = %request_id,
                error = %error,
                "failed to fetch run for download"
            );
            AppError::Internal(
                "une erreur s'est produite veuillez re essayer plus tard".to_string(),
            )
        })?
        .ok_or_else(|| AppError::NotFound("run not found".to_string()))?;

    if run.creator_id != auth.user_id {
        return Err(AppError::Forbidden(
            "run does not belong to caller".to_string(),
        ));
    }

    if run.status.as_str() != "completed" {
        return Err(AppError::Conflict("run is not completed yet".to_string()));
    }

    let object_key = run
        .result_object_key
        .ok_or_else(|| AppError::Internal("result object key is missing".to_string()))?;

    let data = PresignDownloadResponse {
        url: state.storage.build_download_url(&object_key).await?,
        expires_in_seconds: state.config.signed_url_ttl_seconds,
        filename: "lab-result.zip".to_string(),
    };

    Ok(Json(ApiResponse::success(data, Uuid::new_v4().to_string())))
}
