use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::Engine;
use gcp_auth::{provider, TokenProvider};
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::api::{UploadFileItem, UploadTarget},
};

const GCS_API_BASE_URL: &str = "https://storage.googleapis.com";
const IAM_CREDENTIALS_BASE_URL: &str = "https://iamcredentials.googleapis.com";
const RFC3986_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Clone, Debug)]
enum SignedUrlMode {
    Mock,
    IamSignBlob,
}

#[derive(Clone)]
pub struct StorageService {
    http_client: reqwest::Client,
    token_provider: Option<Arc<dyn TokenProvider>>,
    bucket: String,
    ttl_seconds: i64,
    signed_mode: SignedUrlMode,
    signing_service_account: Option<String>,
    local_storage_dir: PathBuf,
    public_base_url: String,
}

#[derive(serde::Deserialize)]
struct IamSignBlobResponse {
    #[serde(rename = "signedBlob")]
    signed_blob: String,
}

impl StorageService {
    pub async fn new(
        bucket: String,
        ttl_seconds: i64,
        signed_mode: String,
        signing_service_account: Option<String>,
        local_storage_dir: String,
        public_base_url: String,
    ) -> Result<Self, AppError> {
        let mode = match signed_mode.as_str() {
            "mock" => SignedUrlMode::Mock,
            _ => SignedUrlMode::IamSignBlob,
        };

        let bucket = sanitize_bucket_name(&bucket)?;
        let token_provider = match mode {
            SignedUrlMode::Mock => None,
            SignedUrlMode::IamSignBlob => Some(provider().await.map_err(|e| {
                AppError::Internal(format!("failed to init gcp auth provider: {e}"))
            })?),
        };

        let local_storage_dir = PathBuf::from(local_storage_dir);
        if matches!(mode, SignedUrlMode::Mock) {
            init_local_storage_dir(&local_storage_dir).await?;
        }

        Ok(Self {
            http_client: reqwest::Client::new(),
            token_provider,
            bucket,
            ttl_seconds,
            signed_mode: mode,
            signing_service_account,
            local_storage_dir,
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
        })
    }

    pub async fn build_upload_targets(
        &self,
        request_id: Uuid,
        files: &[UploadFileItem],
    ) -> Result<Vec<UploadTarget>, AppError> {
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            let safe_name = sanitize_file_name(&f.name)?;
            let object_key = format!("uploads/{request_id}/{safe_name}");
            let upload_url = self.signed_url("PUT", &object_key).await?;
            out.push(UploadTarget {
                object_key,
                upload_url,
                expires_in_seconds: self.ttl_seconds,
            });
        }
        Ok(out)
    }

    pub async fn build_download_url(&self, object_key: &str) -> Result<String, AppError> {
        self.signed_url("GET", object_key).await
    }

    pub fn is_mock_mode(&self) -> bool {
        matches!(self.signed_mode, SignedUrlMode::Mock)
    }

    pub fn result_object_key(&self, request_id: Uuid) -> String {
        format!("results/{request_id}/lab-result.zip")
    }

    pub async fn put_mock_object(
        &self,
        object_key: &str,
        payload: Vec<u8>,
    ) -> Result<(), AppError> {
        if !matches!(self.signed_mode, SignedUrlMode::Mock) {
            return Err(AppError::Internal(
                "put_mock_object is available only in mock mode".to_string(),
            ));
        }

        let (_key, path) = self.local_object_path(object_key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AppError::Internal(format!("failed to create local storage directory: {e}"))
            })?;
        }
        tokio::fs::write(&path, payload).await.map_err(|e| {
            AppError::Internal(format!("failed to write local storage object: {e}"))
        })?;
        Ok(())
    }

    pub async fn download_object_bytes(&self, object_key: &str) -> Result<Vec<u8>, AppError> {
        let object_key = sanitize_object_key(object_key)?;

        if matches!(self.signed_mode, SignedUrlMode::Mock) {
            let (_key, path) = self.local_object_path(&object_key)?;
            return tokio::fs::read(&path).await.map_err(|e| {
                if e.kind() == ErrorKind::NotFound {
                    AppError::NotFound(format!("local object not found: {object_key}"))
                } else {
                    AppError::Internal(format!("failed to read local storage object: {e}"))
                }
            });
        }

        let token = self
            .access_token()
            .await
            .map_err(|e| AppError::Internal(format!("failed to obtain access token: {e}")))?;

        let encoded_name = percent_encode(object_key.as_bytes(), RFC3986_SET).to_string();
        let url = format!(
            "{}/storage/v1/b/{}/o/{}?alt=media",
            GCS_API_BASE_URL, self.bucket, encoded_name
        );

        let response = self
            .http_client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("failed to call GCS download API: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable body>".to_string());
            return Err(AppError::Internal(format!(
                "GCS download failed: HTTP {} - {}",
                status, body
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("failed to read GCS bytes: {e}")))?;

        Ok(bytes.to_vec())
    }

    pub async fn upload_result_bytes(
        &self,
        object_key: &str,
        payload: Vec<u8>,
        content_type: &str,
    ) -> Result<(), AppError> {
        if matches!(self.signed_mode, SignedUrlMode::Mock) {
            let _ = content_type;
            self.put_mock_object(object_key, payload).await?;
            return Ok(());
        }

        let token = self
            .access_token()
            .await
            .map_err(|e| AppError::Internal(format!("failed to obtain access token: {e}")))?;

        let object_key = sanitize_object_key(object_key)?;
        let encoded_name = percent_encode(object_key.as_bytes(), RFC3986_SET).to_string();

        let url = format!(
            "{}/upload/storage/v1/b/{}/o?uploadType=media&name={}",
            GCS_API_BASE_URL, self.bucket, encoded_name
        );

        let response = self
            .http_client
            .post(url)
            .bearer_auth(token)
            .header("Content-Type", content_type)
            .body(payload)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("failed to call GCS upload API: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable body>".to_string());
            return Err(AppError::Internal(format!(
                "GCS upload failed: HTTP {} - {}",
                status, body
            )));
        }

        Ok(())
    }

    async fn signed_url(&self, method: &str, object_key: &str) -> Result<String, AppError> {
        let object_key = sanitize_object_key(object_key)?;

        match self.signed_mode {
            SignedUrlMode::Mock => Ok(format!(
                "{}/local-storage/{}?X-Altair-Signed=mock&method={}",
                self.public_base_url,
                encode_object_path(&object_key),
                method
            )),
            SignedUrlMode::IamSignBlob => self.signed_url_iam_signblob(method, &object_key).await,
        }
    }

    fn local_object_path(&self, object_key: &str) -> Result<(String, PathBuf), AppError> {
        let object_key = sanitize_object_key(object_key)?;
        let mut path = self.local_storage_dir.clone();
        for segment in object_key.split('/') {
            path.push(segment);
        }
        ensure_under_base(&self.local_storage_dir, &path)?;
        Ok((object_key, path))
    }

    async fn signed_url_iam_signblob(
        &self,
        method: &str,
        object_key: &str,
    ) -> Result<String, AppError> {
        let signing_sa = self.signing_service_account.as_ref().ok_or_else(|| {
            AppError::Internal(
                "GCS_SIGNING_SERVICE_ACCOUNT must be set for iam_signblob mode".to_string(),
            )
        })?;

        let now = chrono::Utc::now();
        let datestamp = now.format("%Y%m%d").to_string();
        let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
        let credential_scope = format!("{}/auto/storage/goog4_request", datestamp);
        let credential = format!("{}/{}", signing_sa, credential_scope);

        let canonical_uri = format!("/{}/{}", self.bucket, encode_object_path(object_key));

        let mut query_params = vec![
            (
                "X-Goog-Algorithm".to_string(),
                "GOOG4-RSA-SHA256".to_string(),
            ),
            ("X-Goog-Credential".to_string(), credential),
            ("X-Goog-Date".to_string(), timestamp.clone()),
            (
                "X-Goog-Expires".to_string(),
                self.ttl_seconds.clamp(1, 604800).to_string(),
            ),
            ("X-Goog-SignedHeaders".to_string(), "host".to_string()),
        ];

        query_params.sort_by(|a, b| a.0.cmp(&b.0));
        let canonical_query = canonical_query_string(&query_params);

        let canonical_headers = "host:storage.googleapis.com\n";
        let signed_headers = "host";
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\nUNSIGNED-PAYLOAD",
            method, canonical_uri, canonical_query, canonical_headers, signed_headers
        );

        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!(
            "GOOG4-RSA-SHA256\n{}\n{}\n{}",
            timestamp, credential_scope, canonical_request_hash
        );

        let signature = self
            .sign_blob_with_iam(signing_sa, string_to_sign.as_bytes())
            .await?;

        Ok(format!(
            "https://storage.googleapis.com{}?{}&X-Goog-Signature={}",
            canonical_uri, canonical_query, signature
        ))
    }

    async fn sign_blob_with_iam(
        &self,
        service_account: &str,
        bytes: &[u8],
    ) -> Result<String, AppError> {
        let token = self
            .access_token()
            .await
            .map_err(|e| AppError::Internal(format!("failed to obtain access token: {e}")))?;

        let url = format!(
            "{}/v1/projects/-/serviceAccounts/{}:signBlob",
            IAM_CREDENTIALS_BASE_URL,
            percent_encode(service_account.as_bytes(), RFC3986_SET)
        );

        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let response = self
            .http_client
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({"payload": payload_b64}))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("failed to call IAM signBlob: {e}")))?;

        if response.status() != StatusCode::OK {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable body>".to_string());
            return Err(AppError::Internal(format!(
                "IAM signBlob failed: HTTP {} - {}",
                status, body
            )));
        }

        let body: IamSignBlobResponse = response.json().await.map_err(|e| {
            AppError::Internal(format!("failed to parse IAM signBlob response: {e}"))
        })?;

        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(body.signed_blob)
            .map_err(|e| AppError::Internal(format!("failed to decode signedBlob: {e}")))?;

        Ok(hex::encode(sig_bytes))
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let provider = self
            .token_provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("token provider unavailable in mock mode"))?;
        let token = provider
            .token(&[
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/iam",
            ])
            .await?;
        Ok(token.as_str().to_string())
    }
}

fn canonical_query_string(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encode(k.as_bytes(), RFC3986_SET),
                percent_encode(v.as_bytes(), RFC3986_SET)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_object_path(object_key: &str) -> String {
    object_key
        .split('/')
        .map(|segment| percent_encode(segment.as_bytes(), RFC3986_SET).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitize_file_name(name: &str) -> Result<String, AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "file name cannot be empty".to_string(),
        ));
    }

    if trimmed.contains("../") || trimmed.contains("..\\") || trimmed.starts_with('/') {
        return Err(AppError::BadRequest("invalid file name/path".to_string()));
    }

    Ok(trimmed.replace(' ', "-"))
}

fn sanitize_object_key(object_key: &str) -> Result<String, AppError> {
    let trimmed = object_key.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "object key cannot be empty".to_string(),
        ));
    }
    if trimmed.starts_with('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(AppError::BadRequest("invalid object key".to_string()));
    }

    let allowed_prefix = trimmed.starts_with("uploads/") || trimmed.starts_with("results/");
    if !allowed_prefix {
        return Err(AppError::BadRequest(
            "object key must be under uploads/ or results/".to_string(),
        ));
    }

    if trimmed
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(AppError::BadRequest("invalid object key".to_string()));
    }

    Ok(trimmed.to_string())
}

async fn init_local_storage_dir(base: &Path) -> Result<(), AppError> {
    tokio::fs::create_dir_all(base.join("uploads"))
        .await
        .map_err(|e| AppError::Internal(format!("failed to create local uploads dir: {e}")))?;
    tokio::fs::create_dir_all(base.join("results"))
        .await
        .map_err(|e| AppError::Internal(format!("failed to create local results dir: {e}")))?;
    Ok(())
}

fn ensure_under_base(base: &Path, path: &Path) -> Result<(), AppError> {
    if path.starts_with(base) {
        return Ok(());
    }

    Err(AppError::BadRequest(
        "local storage path escapes base directory".to_string(),
    ))
}

fn sanitize_bucket_name(value: &str) -> Result<String, AppError> {
    let bucket = value.trim().to_ascii_lowercase();
    if bucket.len() < 3 || bucket.len() > 63 {
        return Err(AppError::BadRequest("invalid bucket length".to_string()));
    }

    if !bucket.chars().all(|ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '_'
    }) {
        return Err(AppError::BadRequest(
            "bucket contains invalid characters".to_string(),
        ));
    }

    if bucket.contains('/') || bucket.contains(':') || bucket.contains('?') || bucket.contains('#')
    {
        return Err(AppError::BadRequest(
            "bucket must not look like a URL/path".to_string(),
        ));
    }

    let starts_valid = bucket
        .chars()
        .next()
        .map(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        .unwrap_or(false);
    let ends_valid = bucket
        .chars()
        .last()
        .map(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        .unwrap_or(false);

    if !starts_valid || !ends_valid {
        return Err(AppError::BadRequest(
            "bucket must start/end with letter or digit".to_string(),
        ));
    }

    Ok(bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_object_key_rejects_escape() {
        assert!(sanitize_object_key("../evil").is_err());
        assert!(sanitize_object_key("/abs/path").is_err());
        assert!(sanitize_object_key("uploads/abc/..").is_err());
        assert!(sanitize_object_key("uploads/abc\\file.zip").is_err());
    }

    #[test]
    fn sanitize_object_key_accepts_allowed_prefix() {
        assert!(sanitize_object_key("uploads/abc/file.zip").is_ok());
        assert!(sanitize_object_key("results/abc/lab-result.zip").is_ok());
    }

    #[test]
    fn canonical_query_is_sorted_and_encoded() {
        let mut params = vec![
            ("b".to_string(), "hello world".to_string()),
            ("a".to_string(), "x/y".to_string()),
        ];
        params.sort_by(|x, y| x.0.cmp(&y.0));
        let q = canonical_query_string(&params);
        assert!(q.starts_with("a="));
        assert!(q.contains("hello%20world"));
    }

    #[tokio::test]
    async fn mock_storage_writes_and_reads_disk() {
        let dir = std::env::temp_dir().join(format!("altair-ia-ms-{}", Uuid::new_v4()));
        let storage = StorageService::new(
            "altair-ia-labs".to_string(),
            600,
            "mock".to_string(),
            None,
            dir.to_string_lossy().to_string(),
            "http://localhost:3011".to_string(),
        )
        .await
        .expect("storage initializes");

        storage
            .put_mock_object("uploads/test/source.zip", b"source".to_vec())
            .await
            .expect("write succeeds");
        let bytes = storage
            .download_object_bytes("uploads/test/source.zip")
            .await
            .expect("read succeeds");
        assert_eq!(bytes, b"source");

        let url = storage
            .build_download_url("results/test/lab-result.zip")
            .await
            .expect("mock URL builds");
        assert_eq!(
            url,
            "http://localhost:3011/local-storage/results/test/lab-result.zip?X-Altair-Signed=mock&method=GET"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
