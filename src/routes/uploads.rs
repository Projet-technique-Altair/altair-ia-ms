use axum::{extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth_creator::extract_auth_context,
    models::api::{ApiResponse, PresignUploadRequest, PresignUploadResponse},
    services::file_policy::is_allowed_upload_name,
    state::AppState,
};

const MAX_FILE_SIZE_BYTES: u64 = 30 * 1024 * 1024;
const MAX_TOTAL_SIZE_BYTES: u64 = 120 * 1024 * 1024;
const MAX_FILES: usize = 100;

pub async fn presign_uploads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PresignUploadRequest>,
) -> Result<Json<ApiResponse<PresignUploadResponse>>, AppError> {
    let auth = extract_auth_context(&headers, state.config.require_creator_role)?;

    if payload.files.is_empty() {
        return Err(AppError::BadRequest(
            "files list cannot be empty".to_string(),
        ));
    }

    if payload.files.len() > MAX_FILES {
        return Err(AppError::BadRequest(format!(
            "too many files: max is {MAX_FILES}"
        )));
    }

    let mut total_size = 0u64;
    for file in &payload.files {
        if !is_allowed_upload_name(&file.name) {
            return Err(AppError::UnsupportedFileType(format!(
                "unsupported file: {}",
                file.name
            )));
        }

        if let Some(content_type) = &file.content_type {
            if !is_allowed_content_type(content_type) {
                return Err(AppError::UnsupportedFileType(format!(
                    "unsupported content_type for {}: {}",
                    file.name, content_type
                )));
            }
        }

        let file_size = file.size_bytes.unwrap_or(0);
        if file_size > MAX_FILE_SIZE_BYTES {
            return Err(AppError::FileTooLarge(format!(
                "file {} exceeds 30MB limit",
                file.name
            )));
        }
        total_size = total_size.saturating_add(file_size);
    }

    if total_size > MAX_TOTAL_SIZE_BYTES {
        return Err(AppError::FileTooLarge(
            "total payload exceeds 120MB limit".to_string(),
        ));
    }

    let request_id = payload
        .request_id
        .as_deref()
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap_or_else(Uuid::new_v4);

    let uploads = state
        .storage
        .build_upload_targets(request_id, &payload.files)
        .await?;

    let object_keys = uploads
        .iter()
        .map(|target| target.object_key.clone())
        .collect::<Vec<_>>();

    state
        .run_uploads_repo
        .register_uploaded_objects(request_id, &auth.user_id, &object_keys)
        .await
        .map_err(|error| {
            tracing::error!(
                request_id = %request_id,
                user_id = %auth.user_id,
                error = %error,
                "failed to persist run upload references"
            );
            AppError::Internal(
                "une erreur s'est produite veuillez re essayer plus tard".to_string(),
            )
        })?;

    let data = PresignUploadResponse {
        request_id: request_id.to_string(),
        uploads,
    };

    Ok(Json(ApiResponse::success(data, Uuid::new_v4().to_string())))
}

fn is_allowed_content_type(ct: &str) -> bool {
    let lower = ct.to_ascii_lowercase();
    lower == "application/zip"
        || lower.starts_with("text/")
        || lower == "application/json"
        || lower == "application/javascript"
        || lower == "text/javascript"
        || lower == "application/x-javascript"
        || lower == "application/x-yaml"
        || lower == "application/yaml"
}

#[cfg(test)]
mod tests {
    use super::is_allowed_content_type;
    use crate::services::file_policy::is_allowed_upload_name;

    #[test]
    fn upload_name_allowlist() {
        assert!(is_allowed_upload_name("lab.zip"));
        assert!(is_allowed_upload_name("README.md"));
        assert!(is_allowed_upload_name("app/main.py"));
        assert!(!is_allowed_upload_name("bin/malware.exe"));
    }

    #[test]
    fn content_type_allowlist() {
        assert!(is_allowed_content_type("application/zip"));
        assert!(is_allowed_content_type("text/plain"));
        assert!(is_allowed_content_type("application/javascript"));
        assert!(is_allowed_content_type("text/javascript"));
        assert!(is_allowed_content_type("application/x-javascript"));
        assert!(!is_allowed_content_type("application/x-msdownload"));
    }
}
