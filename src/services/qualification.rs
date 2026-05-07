use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::{
    error::AppError,
    models::{api::QualificationResponse, lab::LabType},
    services::{
        llm::{estimate_tokens, LlmClient},
        llm_config::TOKEN_BUDGET_WARNING,
        prompts::{build_qualification_prompt, PromptLabType},
        run_processor_support::{
            parse_zip_to_base_lab_files, render_base_lab_block, truncate_base_lab_files,
            BaseLabFile,
        },
        storage::StorageService,
    },
};

pub struct QualificationInput<'a> {
    pub request_id: Uuid,
    pub lab_type: LabType,
    pub lab_request_xml: &'a str,
    pub base_lab_block: Option<&'a str>,
}

pub async fn qualify_lab_request(
    llm: &LlmClient,
    input: QualificationInput<'_>,
) -> Result<QualificationResponse, AppError> {
    let prompt_type = match input.lab_type {
        LabType::Web => PromptLabType::Web,
        LabType::Terminal => PromptLabType::Terminal,
    };

    let prompt_bundle = build_qualification_prompt(prompt_type).map_err(|error| {
        AppError::Internal(format!("failed to load qualification prompt: {error}"))
    })?;
    let prompt = build_prompt(
        &prompt_bundle.system,
        input.lab_request_xml,
        input.base_lab_block,
    );

    tracing::info!(
        request_id = %input.request_id,
        lab_type = input.lab_type.as_str(),
        qualification_playbook_chars = prompt_bundle.playbook_chars,
        request_chars = input.lab_request_xml.chars().count(),
        has_base_lab = input.base_lab_block.is_some(),
        "dispatching lab qualification"
    );

    let raw = llm
        .advise_with_context(&prompt, "qualification", input.request_id)
        .await
        .map_err(|error| {
            AppError::AiTemporarilyUnavailable(format!("lab qualification failed: {error}"))
        })?;

    if raw.starts_with("(local-fallback)") {
        return Ok(local_qualification(input.lab_request_xml));
    }

    match parse_and_validate_qualification(&raw) {
        Ok(report) => Ok(report),
        Err(validation_error) => {
            let retry_prompt = build_retry_prompt(&prompt_bundle.system, &validation_error, &raw);
            let retry_raw = llm
                .advise_with_context(&retry_prompt, "qualification", input.request_id)
                .await
                .map_err(|error| {
                    AppError::AiTemporarilyUnavailable(format!(
                        "lab qualification retry failed: {error}"
                    ))
                })?;

            parse_and_validate_qualification(&retry_raw).map_err(|error| {
                AppError::Internal(format!(
                    "lab qualification response did not match schema: {error}"
                ))
            })
        }
    }
}

pub fn qualification_allows_generation(qualification: &QualificationResponse) -> bool {
    qualification.verdict == "compatible"
        && qualification.compatible_altair
        && qualification.respecte_conditions
        && qualification.blocages.is_empty()
}

pub async fn build_qualification_base_lab_block(
    storage: &StorageService,
    base_lab_ref: &str,
    source_objects: &[String],
) -> Result<Option<String>, AppError> {
    if source_objects.is_empty() {
        return Ok(None);
    }

    let mut by_path: BTreeMap<String, BaseLabFile> = BTreeMap::new();

    for object_key in source_objects {
        let bytes = storage.download_object_bytes(object_key).await?;

        if object_key.to_ascii_lowercase().ends_with(".zip") {
            let files = parse_zip_to_base_lab_files(&bytes).map_err(|error| {
                AppError::BadRequest(format!(
                    "failed to parse uploaded source zip `{object_key}`: {error}"
                ))
            })?;

            for file in files {
                by_path.insert(file.path.clone(), file);
            }
            continue;
        }

        let path = source_object_to_base_path(object_key);
        let content = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => format!(
                "[binary file omitted: {} bytes, path={}]",
                error.as_bytes().len(),
                path
            ),
        };

        by_path.insert(
            path.clone(),
            BaseLabFile {
                path,
                estimated_tokens: estimate_tokens(&content),
                content,
            },
        );
    }

    if by_path.is_empty() {
        return Err(AppError::BadRequest(
            "no usable files extracted from uploaded sources".to_string(),
        ));
    }

    let (selected, excluded) =
        truncate_base_lab_files(by_path.into_values().collect(), TOKEN_BUDGET_WARNING);
    if !excluded.is_empty() {
        tracing::info!(
            base_lab_ref,
            excluded_source_files = ?excluded,
            "excluded source files from qualification base_lab context"
        );
    }

    Ok(Some(render_base_lab_block(base_lab_ref, &selected)))
}

fn build_prompt(system: &str, lab_request_xml: &str, base_lab_block: Option<&str>) -> String {
    let base_lab = base_lab_block
        .filter(|block| !block.trim().is_empty())
        .map(|block| format!("\n\nSOURCE DE VARIANTE A QUALIFIER :\n{block}"))
        .unwrap_or_default();

    format!(
        "{system}\n\nDEMANDE A QUALIFIER :\n{lab_request_xml}{base_lab}\n\nRetourne uniquement le JSON de qualification."
    )
}

fn source_object_to_base_path(object_key: &str) -> String {
    let normalized = object_key.trim().replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("uploads/") {
        if let Some((_, path)) = rest.split_once('/') {
            let cleaned = path.trim_matches('/');
            if !cleaned.is_empty() {
                return cleaned.to_string();
            }
        }
    }

    normalized
        .rsplit('/')
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("source-file.txt")
        .to_string()
}

fn build_retry_prompt(system: &str, validation_error: &str, raw: &str) -> String {
    format!(
        r#"{system}

La sortie precedente est invalide.

Erreur de validation : {validation_error}

Corrige uniquement le JSON en respectant exactement le contrat de qualification.
Ne rajoute aucun texte autour du JSON.

Sortie precedente :
{raw}"#
    )
}

pub(crate) fn parse_and_validate_qualification(raw: &str) -> Result<QualificationResponse, String> {
    let json_text = extract_json_object(raw).ok_or_else(|| "missing JSON object".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(json_text).map_err(|error| format!("invalid JSON: {error}"))?;

    validate_exact_keys(&value)?;

    let mut parsed: QualificationResponse =
        serde_json::from_value(value).map_err(|error| format!("invalid schema: {error}"))?;

    parsed.verdict = parsed.verdict.trim().to_ascii_lowercase();
    parsed.resume_utilisateur = parsed.resume_utilisateur.trim().to_string();
    parsed.blocages = normalize_string_list(parsed.blocages);
    parsed.adaptations_requises = normalize_string_list(parsed.adaptations_requises);

    if !matches!(
        parsed.verdict.as_str(),
        "compatible" | "needs_adaptation" | "refused"
    ) {
        return Err(format!("unsupported verdict `{}`", parsed.verdict));
    }

    if parsed.resume_utilisateur.is_empty() {
        return Err("resume_utilisateur must not be empty".to_string());
    }

    if parsed.verdict == "compatible" {
        if !parsed.blocages.is_empty() {
            return Err("compatible verdict requires empty blocages".to_string());
        }
        if !parsed.compatible_altair || !parsed.respecte_conditions {
            return Err(
                "compatible verdict requires compatible_altair and respecte_conditions true"
                    .to_string(),
            );
        }
    }

    if parsed.verdict == "refused" && parsed.blocages.is_empty() {
        return Err("refused verdict requires at least one blocage".to_string());
    }

    Ok(parsed)
}

fn validate_exact_keys(value: &serde_json::Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "qualification response must be a JSON object".to_string())?;
    let expected = [
        "verdict",
        "compatible_altair",
        "respecte_conditions",
        "blocages",
        "adaptations_requises",
        "resume_utilisateur",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();

    if actual != expected {
        return Err(format!(
            "qualification keys mismatch: expected {:?}, got {:?}",
            expected, actual
        ));
    }

    Ok(())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if start > end {
        return None;
    }
    Some(&raw[start..=end])
}

fn local_qualification(lab_request_xml: &str) -> QualificationResponse {
    let lowered = lab_request_xml.to_ascii_lowercase();
    let blockers = [
        (
            "docker-compose",
            "La demande suppose docker-compose, non supporte par le runtime Altair.",
        ),
        (
            "privileged",
            "La demande semble demander un mode privileged ou des privileges host.",
        ),
        (
            "plusieurs ports",
            "La demande mentionne plusieurs ports publics.",
        ),
        (
            "multiple public",
            "La demande mentionne plusieurs points d'entree publics.",
        ),
        (
            "service externe obligatoire",
            "La demande depend d'un service externe obligatoire.",
        ),
    ]
    .into_iter()
    .filter_map(|(needle, message)| lowered.contains(needle).then(|| message.to_string()))
    .collect::<Vec<_>>();

    if !blockers.is_empty() {
        return QualificationResponse {
            verdict: "refused".to_string(),
            compatible_altair: false,
            respecte_conditions: false,
            blocages: blockers,
            adaptations_requises: vec![
                "Adapter la demande pour un seul container Altair sans privilege host.".to_string(),
            ],
            resume_utilisateur:
                "Qualification locale : la demande contient des contraintes incompatibles avec Altair."
                    .to_string(),
        };
    }

    QualificationResponse {
        verdict: "compatible".to_string(),
        compatible_altair: true,
        respecte_conditions: true,
        blocages: Vec::new(),
        adaptations_requises: Vec::new(),
        resume_utilisateur:
            "Qualification locale : aucun blocage evident n'a ete detecte avant generation."
                .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_and_validate_qualification, qualification_allows_generation};
    use crate::models::api::QualificationResponse;

    #[test]
    fn parses_valid_qualification() {
        let raw = r#"{
          "verdict": "compatible",
          "compatible_altair": true,
          "respecte_conditions": true,
          "blocages": [],
          "adaptations_requises": [],
          "resume_utilisateur": "Generation possible."
        }"#;

        let parsed = parse_and_validate_qualification(raw).expect("must parse");
        assert_eq!(parsed.verdict, "compatible");
    }

    #[test]
    fn rejects_extra_keys() {
        let raw = r#"{
          "verdict": "compatible",
          "compatible_altair": true,
          "respecte_conditions": true,
          "blocages": [],
          "adaptations_requises": [],
          "resume_utilisateur": "Generation possible.",
          "extra": true
        }"#;

        let err = parse_and_validate_qualification(raw).expect_err("must reject");
        assert!(err.contains("keys mismatch"));
    }

    #[test]
    fn rejects_refused_without_blockers() {
        let raw = r#"{
          "verdict": "refused",
          "compatible_altair": false,
          "respecte_conditions": false,
          "blocages": [],
          "adaptations_requises": [],
          "resume_utilisateur": "Non compatible."
        }"#;

        let err = parse_and_validate_qualification(raw).expect_err("must reject");
        assert!(err.contains("refused"));
    }

    #[test]
    fn qualification_gate_allows_only_compatible() {
        let ok = QualificationResponse {
            verdict: "compatible".to_string(),
            compatible_altair: true,
            respecte_conditions: true,
            blocages: Vec::new(),
            adaptations_requises: Vec::new(),
            resume_utilisateur: "ok".to_string(),
        };
        assert!(qualification_allows_generation(&ok));

        let refused = QualificationResponse {
            verdict: "needs_adaptation".to_string(),
            compatible_altair: true,
            respecte_conditions: false,
            blocages: Vec::new(),
            adaptations_requises: vec!["adapter".to_string()],
            resume_utilisateur: "non".to_string(),
        };
        assert!(!qualification_allows_generation(&refused));
    }
}
