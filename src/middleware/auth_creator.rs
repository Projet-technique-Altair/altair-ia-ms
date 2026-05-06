use axum::http::HeaderMap;

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: String,
}

pub fn extract_auth_context(
    headers: &HeaderMap,
    require_creator_role: bool,
) -> Result<AuthContext, AppError> {
    let user_id = headers
        .get("x-altair-user-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("x-user-id")
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.trim().is_empty())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            if require_creator_role {
                None
            } else {
                Some("local-test-user".to_string())
            }
        })
        .ok_or_else(|| AppError::Unauthorized("missing x-altair-user-id header".to_string()))?;

    if require_creator_role {
        let role_header = headers
            .get("x-altair-roles")
            .and_then(|v| v.to_str().ok())
            .or_else(|| headers.get("x-altair-role").and_then(|v| v.to_str().ok()))
            .or_else(|| headers.get("x-user-roles").and_then(|v| v.to_str().ok()))
            .or_else(|| headers.get("x-user-role").and_then(|v| v.to_str().ok()))
            .unwrap_or_default()
            .to_ascii_lowercase();

        let is_creator = role_header
            .split(',')
            .map(|s| s.trim())
            .any(|role| role == "creator");

        if !is_creator {
            return Err(AppError::Forbidden(
                "creator role required for this endpoint".to_string(),
            ));
        }
    }

    Ok(AuthContext { user_id })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::extract_auth_context;

    #[test]
    fn fallback_user_is_allowed_when_creator_role_not_required() {
        let headers = HeaderMap::new();
        let auth = extract_auth_context(&headers, false).expect("must allow local fallback");
        assert_eq!(auth.user_id, "local-test-user");
    }

    #[test]
    fn missing_user_is_rejected_when_creator_role_required() {
        let headers = HeaderMap::new();
        let err = extract_auth_context(&headers, true).expect_err("must reject");
        assert!(err.to_string().contains("missing x-altair-user-id header"));
    }

    #[test]
    fn altair_headers_are_preferred() {
        let mut headers = HeaderMap::new();
        headers.insert("x-altair-user-id", "creator-1".parse().expect("header"));
        headers.insert("x-altair-roles", "creator,learner".parse().expect("header"));

        let auth = extract_auth_context(&headers, true).expect("must accept");
        assert_eq!(auth.user_id, "creator-1");
    }

    #[test]
    fn legacy_headers_still_work_for_compatibility() {
        let mut headers = HeaderMap::new();
        headers.insert("x-user-id", "creator-legacy".parse().expect("header"));
        headers.insert("x-user-roles", "creator".parse().expect("header"));

        let auth = extract_auth_context(&headers, true).expect("must accept");
        assert_eq!(auth.user_id, "creator-legacy");
    }
}
