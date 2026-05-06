use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ApiMeta {
    pub request_id: String,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
    pub retryable: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T>
where
    T: Serialize,
{
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<ApiError>,
    pub meta: ApiMeta,
}

impl<T> ApiResponse<T>
where
    T: Serialize,
{
    pub fn success(data: T, request_id: String) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            meta: ApiMeta {
                request_id,
                timestamp: Utc::now().to_rfc3339(),
            },
        }
    }

    pub fn error(error: ApiError, request_id: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            meta: ApiMeta {
                request_id,
                timestamp: Utc::now().to_rfc3339(),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PresignUploadRequest {
    pub request_id: Option<String>,
    pub files: Vec<UploadFileItem>,
}

#[derive(Debug, Deserialize)]
pub struct UploadFileItem {
    pub name: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct PresignUploadResponse {
    pub request_id: String,
    pub uploads: Vec<UploadTarget>,
}

#[derive(Debug, Serialize)]
pub struct UploadTarget {
    pub object_key: String,
    pub upload_url: String,
    pub expires_in_seconds: i64,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteStructuredRequest {
    pub request_id: Option<String>,
    pub mode: String,
    pub lab_type: String,
    pub difficulty: Option<String>,
    pub stack_main: String,
    pub goal: String,
    pub functional_description: String,
    pub security_description: String,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub forced_dependencies: Vec<String>,
    pub visual_description: Option<String>,
    pub options: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteResponse {
    pub request_id: String,
    pub status: String,
    pub status_url: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct RunStatusResponse {
    pub request_id: String,
    pub status: String,
    pub mode_selected: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub retryable: bool,
    pub used_model_fallback: bool,
    pub estimated_input_tokens: Option<u64>,
    pub actual_input_tokens: Option<u64>,
    pub actual_output_tokens: Option<u64>,
    pub excluded_source_objects: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PresignDownloadResponse {
    pub url: String,
    pub expires_in_seconds: i64,
    pub filename: String,
}
