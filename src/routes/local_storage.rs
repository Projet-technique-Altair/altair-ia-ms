use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize)]
pub(crate) struct MockSignedQuery {
    #[serde(rename = "X-Altair-Signed")]
    signed: Option<String>,
    method: Option<String>,
}

pub async fn put_local_object(
    State(state): State<AppState>,
    Path(object_key): Path<String>,
    Query(query): Query<MockSignedQuery>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    ensure_mock_storage_mode(&state)?;
    validate_mock_signed_query(&query, "PUT")?;
    state
        .storage
        .put_mock_object(&object_key, body.to_vec())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_local_object(
    State(state): State<AppState>,
    Path(object_key): Path<String>,
    Query(query): Query<MockSignedQuery>,
) -> Result<Response, AppError> {
    ensure_mock_storage_mode(&state)?;
    validate_mock_signed_query(&query, "GET")?;
    let bytes = state.storage.download_object_bytes(&object_key).await?;
    let content_type = if object_key.ends_with(".zip") {
        "application/zip"
    } else {
        "application/octet-stream"
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    let filename = object_key
        .rsplit('/')
        .next()
        .unwrap_or("download.bin")
        .replace('"', "");
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }

    Ok((headers, bytes).into_response())
}

fn ensure_mock_storage_mode(state: &AppState) -> Result<(), AppError> {
    if state.storage.is_mock_mode() {
        Ok(())
    } else {
        Err(AppError::NotFound("local storage is disabled".to_string()))
    }
}

fn validate_mock_signed_query(
    query: &MockSignedQuery,
    expected_method: &str,
) -> Result<(), AppError> {
    if query.signed.as_deref() != Some("mock") {
        return Err(AppError::Forbidden(
            "missing or invalid mock signed token".to_string(),
        ));
    }

    let Some(method) = query.method.as_deref() else {
        return Err(AppError::BadRequest(
            "missing signed URL method".to_string(),
        ));
    };

    if !method.eq_ignore_ascii_case(expected_method) {
        return Err(AppError::Forbidden(
            "signed URL method mismatch".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_mock_signed_query, MockSignedQuery};

    #[test]
    fn signed_query_requires_mock_marker_and_matching_method() {
        let query = MockSignedQuery {
            signed: Some("mock".to_string()),
            method: Some("GET".to_string()),
        };

        assert!(validate_mock_signed_query(&query, "GET").is_ok());
        assert!(validate_mock_signed_query(&query, "PUT").is_err());
    }

    #[test]
    fn signed_query_rejects_missing_signature() {
        let query = MockSignedQuery {
            signed: None,
            method: Some("GET".to_string()),
        };

        assert!(validate_mock_signed_query(&query, "GET").is_err());
    }
}
