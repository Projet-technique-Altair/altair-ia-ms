use axum::http::HeaderMap;
use chrono::Utc;

use crate::error::AppError;

#[derive(Clone)]
pub struct OidcVerifier {
    http_client: reqwest::Client,
    tokeninfo_url: String,
    expected_audience: String,
    allowed_service_account_email: String,
}

#[derive(Debug, serde::Deserialize)]
struct TokenInfoResponse {
    aud: Option<String>,
    iss: Option<String>,
    exp: Option<String>,
    email: Option<String>,
    email_verified: Option<String>,
}

impl OidcVerifier {
    pub fn new(
        tokeninfo_url: String,
        expected_audience: String,
        allowed_service_account_email: String,
    ) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            tokeninfo_url,
            expected_audience,
            allowed_service_account_email,
        }
    }

    pub async fn verify_headers(&self, headers: &HeaderMap) -> Result<(), AppError> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("missing authorization header".to_string()))?;

        let token = auth
            .strip_prefix("Bearer ")
            .ok_or_else(|| AppError::Unauthorized("invalid bearer token format".to_string()))?;

        self.verify_token(token).await
    }

    pub async fn verify_token(&self, token: &str) -> Result<(), AppError> {
        let response = self
            .http_client
            .get(&self.tokeninfo_url)
            .query(&[("id_token", token)])
            .send()
            .await
            .map_err(|e| {
                AppError::Unauthorized(format!("token verification request failed: {e}"))
            })?;

        if !response.status().is_success() {
            return Err(AppError::Unauthorized(
                "token verification failed".to_string(),
            ));
        }

        let info: TokenInfoResponse = response
            .json()
            .await
            .map_err(|e| AppError::Unauthorized(format!("invalid tokeninfo payload: {e}")))?;

        validate_tokeninfo(
            &info,
            &self.expected_audience,
            &self.allowed_service_account_email,
        )
    }
}

fn validate_tokeninfo(
    info: &TokenInfoResponse,
    expected_audience: &str,
    allowed_service_account_email: &str,
) -> Result<(), AppError> {
    let aud = info
        .aud
        .as_deref()
        .ok_or_else(|| AppError::Unauthorized("token missing aud".to_string()))?;
    if aud != expected_audience {
        return Err(AppError::Forbidden("token audience mismatch".to_string()));
    }

    if let Some(iss) = &info.iss {
        let valid_iss = iss == "accounts.google.com" || iss == "https://accounts.google.com";
        if !valid_iss {
            return Err(AppError::Forbidden("invalid token issuer".to_string()));
        }
    }

    let email = info
        .email
        .as_deref()
        .ok_or_else(|| AppError::Unauthorized("token missing email".to_string()))?;
    if email != allowed_service_account_email {
        return Err(AppError::Forbidden(
            "token service account is not allowed".to_string(),
        ));
    }

    let email_verified = info.email_verified.as_deref().unwrap_or("false");
    if email_verified != "true" {
        return Err(AppError::Forbidden("token email not verified".to_string()));
    }

    let exp_raw = info
        .exp
        .as_deref()
        .ok_or_else(|| AppError::Unauthorized("token missing exp".to_string()))?;
    let exp = exp_raw
        .parse::<i64>()
        .map_err(|_| AppError::Unauthorized("invalid token exp".to_string()))?;

    if exp <= Utc::now().timestamp() {
        return Err(AppError::Unauthorized("token expired".to_string()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_tokeninfo, TokenInfoResponse};

    fn valid() -> TokenInfoResponse {
        TokenInfoResponse {
            aud: Some("https://service.example".to_string()),
            iss: Some("https://accounts.google.com".to_string()),
            exp: Some((chrono::Utc::now().timestamp() + 120).to_string()),
            email: Some("worker-sa@project.iam.gserviceaccount.com".to_string()),
            email_verified: Some("true".to_string()),
        }
    }

    #[test]
    fn validate_tokeninfo_ok() {
        let info = valid();
        let res = validate_tokeninfo(
            &info,
            "https://service.example",
            "worker-sa@project.iam.gserviceaccount.com",
        );
        assert!(res.is_ok());
    }

    #[test]
    fn validate_tokeninfo_rejects_bad_audience() {
        let info = valid();
        let res = validate_tokeninfo(
            &info,
            "https://other.example",
            "worker-sa@project.iam.gserviceaccount.com",
        );
        assert!(res.is_err());
    }
}
