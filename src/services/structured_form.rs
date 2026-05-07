use crate::{
    error::AppError,
    models::{api::ExecuteStructuredRequest, lab::Difficulty, lab::LabType, run::RunMode},
};

const MAX_LIST_ITEMS: usize = 32;
const MAX_ITEM_LENGTH: usize = 180;

#[derive(Debug, Clone)]
pub struct StructuredExecutionInput {
    pub mode: RunMode,
    pub lab_type: LabType,
    pub prompt: String,
}

struct LabRequestPayload {
    mode: RunMode,
    lab_type: LabType,
    difficulty: Difficulty,
    stack_main: String,
    goal: String,
    functional_description: String,
    security_description: String,
    frameworks: Vec<String>,
    forced_dependencies: Vec<String>,
    visual_description: Option<String>,
}

pub fn normalize_structured_execution(
    payload: ExecuteStructuredRequest,
) -> Result<StructuredExecutionInput, AppError> {
    let mode_raw = trim_required("mode", &payload.mode)?;
    let mode = RunMode::parse(&mode_raw)
        .ok_or_else(|| AppError::ModeNotAllowed(format!("unknown mode `{mode_raw}`")))?;

    let lab_type = parse_lab_type(&payload.lab_type)?;
    let stack_main = trim_required("stack_main", &payload.stack_main)?;
    let goal = trim_required("goal", &payload.goal)?;
    let functional_description =
        trim_required("functional_description", &payload.functional_description)?;
    let security_description =
        trim_required("security_description", &payload.security_description)?;
    let difficulty = parse_difficulty(payload.difficulty.as_deref())?;

    let frameworks = normalize_string_list("frameworks", payload.frameworks)?;
    let forced_dependencies =
        normalize_string_list("forced_dependencies", payload.forced_dependencies)?;

    let visual_description = payload
        .visual_description
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    if visual_description.is_some() && !matches!(lab_type, LabType::Web) {
        return Err(AppError::BadRequest(
            "visual_description is allowed only for web lab_type".to_string(),
        ));
    }

    let request_payload = LabRequestPayload {
        mode,
        lab_type,
        difficulty,
        stack_main,
        goal,
        functional_description,
        security_description,
        frameworks,
        forced_dependencies,
        visual_description,
    };
    let prompt = build_lab_request_xml(&request_payload);

    Ok(StructuredExecutionInput {
        mode,
        lab_type,
        prompt,
    })
}

fn build_lab_request_xml(input: &LabRequestPayload) -> String {
    let mode_value = if matches!(input.mode, RunMode::CreateVariant) {
        "variant"
    } else {
        "initial"
    };

    let frameworks_value = list_to_csv(&input.frameworks);
    let forced_dependencies_value = list_to_csv(&input.forced_dependencies);
    let visual_value = input.visual_description.as_deref().unwrap_or("");

    format!(
        "<lab_request>\n  <mode>{}</mode>\n  <lab_type>{}</lab_type>\n  <difficulty>{}</difficulty>\n  <stack_main>{}</stack_main>\n  <goal>{}</goal>\n  <functional_description>{}</functional_description>\n  <security_description>{}</security_description>\n  <frameworks>{}</frameworks>\n  <forced_dependencies>{}</forced_dependencies>\n  <visual_description>{}</visual_description>\n  <hard_constraints>ctf_form_v1</hard_constraints>\n</lab_request>",
        xml_escape(mode_value),
        xml_escape(input.lab_type.as_str()),
        xml_escape(input.difficulty.as_str()),
        xml_escape(&input.stack_main),
        xml_escape(&input.goal),
        xml_escape(&input.functional_description),
        xml_escape(&input.security_description),
        xml_escape(&frameworks_value),
        xml_escape(&forced_dependencies_value),
        xml_escape(visual_value)
    )
}

fn list_to_csv(values: &[String]) -> String {
    values.join(", ")
}

fn parse_lab_type(raw: &str) -> Result<LabType, AppError> {
    let value = trim_required("lab_type", raw)?;
    LabType::parse(&value).ok_or_else(|| {
        AppError::BadRequest(format!(
            "invalid lab_type `{}` (allowed: web, terminal)",
            value.to_ascii_lowercase()
        ))
    })
}

fn parse_difficulty(raw: Option<&str>) -> Result<Difficulty, AppError> {
    let value = raw.unwrap_or("course").trim().to_string();
    Difficulty::parse(&value).ok_or_else(|| {
        AppError::BadRequest(format!(
            "invalid difficulty `{}` (allowed: course, guided, non guided)",
            value.to_ascii_lowercase()
        ))
    })
}

fn trim_required(field: &str, value: &str) -> Result<String, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(format!("{field} is required")));
    }
    Ok(trimmed.to_string())
}

fn normalize_string_list(field: &str, input: Vec<String>) -> Result<Vec<String>, AppError> {
    if input.len() > MAX_LIST_ITEMS {
        return Err(AppError::BadRequest(format!(
            "{field} exceeds max size {MAX_LIST_ITEMS}"
        )));
    }

    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for item in input {
        let normalized = item.trim().to_string();
        if normalized.is_empty() {
            continue;
        }
        if normalized.len() > MAX_ITEM_LENGTH {
            return Err(AppError::BadRequest(format!(
                "{field} entry is too long (max {MAX_ITEM_LENGTH} chars)"
            )));
        }

        let key = normalized.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(normalized);
        }
    }

    Ok(out)
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::normalize_structured_execution;
    use crate::models::api::ExecuteStructuredRequest;

    fn sample() -> ExecuteStructuredRequest {
        ExecuteStructuredRequest {
            request_id: None,
            mode: "generate_from_scratch".to_string(),
            lab_type: "web".to_string(),
            stack_main: "node".to_string(),
            goal: "Teach SQLi".to_string(),
            functional_description: "Login page + vulnerable endpoint".to_string(),
            security_description: "Boolean-based SQL injection in auth flow".to_string(),
            frameworks: vec!["express".to_string()],
            forced_dependencies: vec!["sqlite3".to_string()],
            difficulty: Some("guided".to_string()),
            visual_description: Some("Minimal dark dashboard".to_string()),
            options: None,
        }
    }

    #[test]
    fn normalizes_payload_into_xml_prompt() {
        let parsed = normalize_structured_execution(sample()).expect("must normalize");
        assert_eq!(parsed.mode.as_str(), "generate_from_scratch");
        assert!(parsed.prompt.contains("<lab_request>"));
        assert!(parsed.prompt.contains("<difficulty>guided</difficulty>"));
    }

    #[test]
    fn rejects_visual_for_terminal() {
        let mut payload = sample();
        payload.lab_type = "terminal".to_string();
        let err = normalize_structured_execution(payload).expect_err("must fail");
        assert!(err.to_string().contains("visual_description"));
    }

    #[test]
    fn variant_mode_still_generates_prompt() {
        let mut payload = sample();
        payload.mode = "create_variant".to_string();
        let out = normalize_structured_execution(payload).expect("must normalize");
        assert!(out.prompt.contains("<mode>variant</mode>"));
    }
}
