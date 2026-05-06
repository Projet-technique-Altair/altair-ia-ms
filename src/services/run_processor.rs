use std::{collections::BTreeMap, time::Duration};

use serde::Serialize;
use uuid::Uuid;

use crate::{
    models::lab::{Difficulty, LabType},
    repository::runs_repository::RunsRepository,
    services::{
        llm::{estimate_tokens, LabGenerationInput, LlmClient, LlmError, LlmErrorKind},
        llm_config::{MAX_REQUEUE_CYCLES, MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS},
        prompts::{build_system_prompt, PromptLabType},
        run_processor_support::{
            build_result_zip, extract_unsatisfiable_reason, parse_lab_files,
            parse_zip_to_base_lab_files, render_base_lab_block, truncate_base_lab_files,
            truncate_for_log, validate_generated_lab, BaseLabFile,
        },
        storage::StorageService,
    },
};

const BASE_LAB_TRUNCATION_WARNING_TOKENS: u64 = 120_000;
const LAB_GENERATION_REPAIR_RETRIES: u8 = 1;
const LAB_GENERATION_REPAIR_ATTEMPT: u8 = LAB_GENERATION_REPAIR_RETRIES + 1;

#[derive(Clone)]
pub struct RunProcessor {
    runs_repo: RunsRepository,
    llm: LlmClient,
    storage: StorageService,
    process_timeout_seconds: u64,
    max_attempts: u8,
}

#[derive(Debug)]
struct ProcessFailure {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl ProcessFailure {
    fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Debug)]
struct ProcessSuccess {
    mode: String,
    result_object_key: String,
    used_model_fallback: bool,
}

type ProcessResult<T> = Result<T, ProcessFailure>;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum GenerationMode {
    Initial,
    Variant,
}

#[derive(Debug, Serialize)]
struct PromptLayersMetrics {
    system_chars: usize,
    playbook_chars: usize,
    request_chars: usize,
    base_lab_chars: usize,
}

#[derive(Debug, Serialize)]
struct StageLog<'a> {
    run_id: Uuid,
    mode: GenerationMode,
    stage: &'a str,
    model: &'a str,
    estimated_input_tokens: Option<u64>,
    actual_input_tokens: Option<u64>,
    actual_output_tokens: Option<u64>,
    attempt: u8,
    base_lab_ref: Option<String>,
    base_lab_files_count: usize,
    prompt_layers: PromptLayersMetrics,
    parse_success: Option<bool>,
    validation_success: Option<bool>,
    files_generated: Option<usize>,
    error: Option<String>,
}

impl RunProcessor {
    pub fn new(
        runs_repo: RunsRepository,
        llm: LlmClient,
        storage: StorageService,
        process_timeout_seconds: u64,
        _max_attempts: u8,
    ) -> Self {
        Self {
            runs_repo,
            llm,
            storage,
            process_timeout_seconds: process_timeout_seconds.clamp(30, 3600),
            max_attempts: 1,
        }
    }

    pub async fn process_run(&self, request_id: Uuid) {
        let run_started = match self.runs_repo.mark_running(request_id).await {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    run_id = %request_id,
                    error = %error,
                    "failed to mark run as running"
                );
                return;
            }
        };

        if !run_started {
            return;
        }

        let max_attempts = self.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            let timed_result = tokio::time::timeout(
                Duration::from_secs(self.process_timeout_seconds),
                self.process_run_inner(request_id),
            )
            .await;

            match timed_result {
                Ok(Ok(success)) => {
                    if let Err(error) = self
                        .runs_repo
                        .mark_completed(
                            request_id,
                            &success.result_object_key,
                            &success.mode,
                            success.used_model_fallback,
                        )
                        .await
                    {
                        tracing::error!(
                            run_id = %request_id,
                            error = %error,
                            "failed to persist completed run status"
                        );
                    }
                    return;
                }
                Ok(Err(error)) => {
                    if error.code == "MODEL_UNAVAILABLE" {
                        if let Err(requeue_error) = self
                            .maybe_requeue_model_unavailable(request_id, &error.message)
                            .await
                        {
                            tracing::error!(
                                run_id = %request_id,
                                error = %requeue_error,
                                "failed to requeue model unavailable run"
                            );
                            let _ = self
                                .runs_repo
                                .mark_failed(
                                    request_id,
                                    "MODEL_UNAVAILABLE",
                                    "LLM model unavailable and requeue failed",
                                )
                                .await;
                        }
                        return;
                    }

                    if error.retryable && attempt < max_attempts {
                        tracing::warn!(
                            run_id = %request_id,
                            attempt,
                            max_attempts,
                            code = error.code,
                            message = %error.message,
                            "retryable run processing error, retrying"
                        );
                        continue;
                    }

                    tracing::error!(
                        run_id = %request_id,
                        code = error.code,
                        internal_error = %error.message,
                        "run processing failed"
                    );

                    if let Err(mark_error) = self
                        .runs_repo
                        .mark_failed(request_id, error.code, &error.message)
                        .await
                    {
                        tracing::error!(
                            run_id = %request_id,
                            code = error.code,
                            error = %mark_error,
                            "failed to persist failed run status"
                        );
                    }
                    return;
                }
                Err(_) => {
                    let message = format!(
                        "run exceeded timeout of {} seconds on attempt {}/{}",
                        self.process_timeout_seconds, attempt, max_attempts
                    );
                    if attempt < max_attempts {
                        tracing::warn!(
                            run_id = %request_id,
                            attempt,
                            max_attempts,
                            timeout_seconds = self.process_timeout_seconds,
                            "run timed out, retrying"
                        );
                        continue;
                    }
                    tracing::error!(
                        run_id = %request_id,
                        code = "TIMEOUT",
                        internal_error = %message,
                        "run processing timed out"
                    );

                    if let Err(mark_error) = self
                        .runs_repo
                        .mark_failed(request_id, "TIMEOUT", &message)
                        .await
                    {
                        tracing::error!(
                            run_id = %request_id,
                            error = %mark_error,
                            "failed to persist timeout status"
                        );
                    }
                    return;
                }
            }
        }
    }

    async fn maybe_requeue_model_unavailable(
        &self,
        request_id: Uuid,
        reason: &str,
    ) -> Result<(), String> {
        let run = self
            .runs_repo
            .get_run(request_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "run not found while requeuing".to_string())?;

        let current_cycle = extract_requeue_cycle(run.error_message.as_deref());
        if current_cycle >= MAX_REQUEUE_CYCLES {
            self.runs_repo
                .mark_failed(request_id, "MODEL_UNAVAILABLE", reason)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let next_cycle = current_cycle + 1;
        self.runs_repo
            .mark_requeued_model_unavailable(request_id, next_cycle, reason)
            .await
            .map_err(|e| e.to_string())?;

        let processor = self.clone();
        let runtime_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(MODEL_UNAVAILABLE_RETRY_AFTER_SECONDS));
            runtime_handle.block_on(async move {
                processor.process_run(request_id).await;
            });
        });

        Ok(())
    }

    async fn process_run_inner(&self, request_id: Uuid) -> ProcessResult<ProcessSuccess> {
        let run = self
            .runs_repo
            .get_run(request_id)
            .await
            .map_err(|error| {
                ProcessFailure::new(
                    "REPOSITORY_ERROR",
                    format!("failed to fetch run in repository: {error}"),
                    true,
                )
            })?
            .ok_or_else(|| ProcessFailure::new("RUN_NOT_FOUND", "run not found", false))?;

        let mode = run
            .mode_selected
            .clone()
            .unwrap_or_else(|| "generate_from_scratch".to_string());
        let generation_mode = match mode.as_str() {
            "create_variant" => GenerationMode::Variant,
            _ => GenerationMode::Initial,
        };

        let lab_type = parse_lab_type_from_request(&run.prompt).unwrap_or_else(|| {
            if run.prompt.to_ascii_lowercase().contains("terminal") {
                LabType::Terminal
            } else {
                LabType::Web
            }
        });

        let prompt_bundle = build_system_prompt(
            match lab_type {
                LabType::Web => PromptLabType::Web,
                LabType::Terminal => PromptLabType::Terminal,
            },
            matches!(generation_mode, GenerationMode::Variant),
        )
        .map_err(|error| {
            ProcessFailure::new(
                "PROMPT_BUILD_FAILED",
                format!("failed to load playbook: {error}"),
                false,
            )
        })?;

        let request_layer = if run.prompt.trim_start().starts_with("<lab_request>") {
            run.prompt.clone()
        } else {
            fallback_request_layer(&run.prompt, lab_type, generation_mode)
        };
        let difficulty = parse_difficulty_from_request(&request_layer);

        let (base_lab_block, base_lab_ref, base_lab_files_count, excluded_files, base_lab_chars) =
            if matches!(generation_mode, GenerationMode::Variant) {
                if run.input_refs.is_empty() {
                    return Err(ProcessFailure::new(
                        "INVALID_REQUEST",
                        "create_variant requires source_objects",
                        false,
                    ));
                }

                let loaded_files = self
                    .load_source_objects_as_base_lab_files(&run.input_refs)
                    .await?;
                let (selected, excluded) =
                    truncate_base_lab_files(loaded_files, BASE_LAB_TRUNCATION_WARNING_TOKENS);
                if selected.iter().map(|f| f.estimated_tokens).sum::<u64>()
                    > BASE_LAB_TRUNCATION_WARNING_TOKENS
                {
                    tracing::warn!(
                        run_id = %request_id,
                        source_objects_count = run.input_refs.len(),
                        "base_lab core files still exceed truncation threshold"
                    );
                }

                let base_ref = format!("source_objects:{}", run.input_refs.len());
                let block = render_base_lab_block(&base_ref, &selected);
                let count = selected.len();
                let chars = block.chars().count();
                (block, Some(base_ref), count, excluded, chars)
            } else {
                (String::new(), None, 0, Vec::new(), 0usize)
            };

        let system_layer = prompt_bundle.system;

        let mut user_payload = if base_lab_block.is_empty() {
            request_layer.clone()
        } else {
            format!("{}\n\n{}", request_layer, base_lab_block)
        };

        let prompt_layers = PromptLayersMetrics {
            system_chars: system_layer.chars().count(),
            playbook_chars: prompt_bundle.layer2_chars,
            request_chars: request_layer.chars().count(),
            base_lab_chars,
        };

        let estimate = self
            .llm
            .count_tokens(request_id, &system_layer, &user_payload)
            .await
            .map_err(|e| map_llm_error("PRE_DISPATCH_FAILED", e))?;

        if let Err(error) = self.llm.enforce_token_budget(request_id, &estimate) {
            return Err(map_llm_error("PROMPT_TOO_LARGE", error));
        }

        if let Err(error) = self
            .runs_repo
            .update_observability(
                request_id,
                Some(estimate.input_tokens),
                None,
                None,
                &excluded_files,
            )
            .await
        {
            tracing::warn!(run_id = %request_id, error = %error, "failed to persist estimated tokens");
        }

        log_stage(StageLog {
            run_id: request_id,
            mode: generation_mode,
            stage: "pre_dispatch",
            model: self.llm.model_name(),
            estimated_input_tokens: Some(estimate.input_tokens),
            actual_input_tokens: None,
            actual_output_tokens: None,
            attempt: 1,
            base_lab_ref: base_lab_ref.clone(),
            base_lab_files_count,
            prompt_layers,
            parse_success: None,
            validation_success: None,
            files_generated: None,
            error: None,
        });

        log_stage(StageLog {
            run_id: request_id,
            mode: generation_mode,
            stage: "api_call",
            model: self.llm.model_name(),
            estimated_input_tokens: Some(estimate.input_tokens),
            actual_input_tokens: None,
            actual_output_tokens: None,
            attempt: 1,
            base_lab_ref: base_lab_ref.clone(),
            base_lab_files_count,
            prompt_layers: PromptLayersMetrics {
                system_chars: system_layer.chars().count(),
                playbook_chars: prompt_bundle.layer2_chars,
                request_chars: request_layer.chars().count(),
                base_lab_chars,
            },
            parse_success: None,
            validation_success: None,
            files_generated: None,
            error: None,
        });

        let mut model_output = self
            .llm
            .generate_lab_files(&LabGenerationInput {
                run_id: request_id,
                mode: mode.clone(),
                system: system_layer.clone(),
                user_message: user_payload.clone(),
            })
            .await
            .map_err(|e| map_llm_error("MODEL_CALL_FAILED", e))?;
        let mut used_model_fallback = model_output.used_fallback;

        if let Some(reason) = extract_unsatisfiable_reason(&model_output.raw_response) {
            return Err(ProcessFailure::new("UNSATISFIABLE_REQUEST", reason, false));
        }

        let mut generation_attempt = 1u8;
        let mut files = match parse_lab_files(&model_output.raw_response) {
            Ok(files) => {
                log_stage(StageLog {
                    run_id: request_id,
                    mode: generation_mode,
                    stage: "parse",
                    model: model_output.model.as_str(),
                    estimated_input_tokens: Some(estimate.input_tokens),
                    actual_input_tokens: model_output.usage.actual_input_tokens,
                    actual_output_tokens: model_output.usage.actual_output_tokens,
                    attempt: generation_attempt,
                    base_lab_ref: base_lab_ref.clone(),
                    base_lab_files_count,
                    prompt_layers: PromptLayersMetrics {
                        system_chars: system_layer.chars().count(),
                        playbook_chars: prompt_bundle.layer2_chars,
                        request_chars: request_layer.chars().count(),
                        base_lab_chars,
                    },
                    parse_success: Some(true),
                    validation_success: None,
                    files_generated: Some(files.len()),
                    error: None,
                });
                files
            }
            Err(parse_error) => {
                let failure_message = format!(
                    "{parse_error}. Raw(first500): {}",
                    truncate_for_log(&model_output.raw_response, 500)
                );

                log_lab_generation_failure(
                    request_id,
                    generation_mode,
                    "parse",
                    model_output.model.as_str(),
                    Some(estimate.input_tokens),
                    model_output.usage.actual_input_tokens,
                    model_output.usage.actual_output_tokens,
                    generation_attempt,
                    base_lab_ref.clone(),
                    base_lab_files_count,
                    system_layer.chars().count(),
                    prompt_bundle.layer2_chars,
                    request_layer.chars().count(),
                    base_lab_chars,
                    Some(false),
                    None,
                    None,
                    &failure_message,
                    &model_output.raw_response,
                    false,
                );

                user_payload = build_lab_repair_payload(
                    &user_payload,
                    "parsing",
                    &failure_message,
                    &model_output.raw_response,
                );

                generation_attempt = LAB_GENERATION_REPAIR_ATTEMPT;
                log_lab_repair_dispatch(
                    request_id,
                    generation_mode,
                    model_output.model.as_str(),
                    Some(estimate.input_tokens),
                    model_output.usage.actual_input_tokens,
                    model_output.usage.actual_output_tokens,
                    generation_attempt,
                    base_lab_ref.clone(),
                    base_lab_files_count,
                    system_layer.chars().count(),
                    prompt_bundle.layer2_chars,
                    request_layer.chars().count(),
                    base_lab_chars,
                    Some(false),
                    None,
                    None,
                    &failure_message,
                );

                let repair_provider = model_output.provider.clone();
                model_output = self
                    .llm
                    .generate_lab_files_with_preferred_provider(
                        &LabGenerationInput {
                            run_id: request_id,
                            mode: mode.clone(),
                            system: system_layer.clone(),
                            user_message: user_payload.clone(),
                        },
                        repair_provider.as_str(),
                    )
                    .await
                    .map_err(|e| map_llm_error("MODEL_CALL_FAILED", e))?;
                used_model_fallback |= model_output.used_fallback;

                if let Some(reason) = extract_unsatisfiable_reason(&model_output.raw_response) {
                    return Err(ProcessFailure::new("UNSATISFIABLE_REQUEST", reason, false));
                }

                match parse_lab_files(&model_output.raw_response) {
                    Ok(files) => {
                        log_stage(StageLog {
                            run_id: request_id,
                            mode: generation_mode,
                            stage: "parse",
                            model: model_output.model.as_str(),
                            estimated_input_tokens: Some(estimate.input_tokens),
                            actual_input_tokens: model_output.usage.actual_input_tokens,
                            actual_output_tokens: model_output.usage.actual_output_tokens,
                            attempt: generation_attempt,
                            base_lab_ref: base_lab_ref.clone(),
                            base_lab_files_count,
                            prompt_layers: PromptLayersMetrics {
                                system_chars: system_layer.chars().count(),
                                playbook_chars: prompt_bundle.layer2_chars,
                                request_chars: request_layer.chars().count(),
                                base_lab_chars,
                            },
                            parse_success: Some(true),
                            validation_success: None,
                            files_generated: Some(files.len()),
                            error: None,
                        });
                        files
                    }
                    Err(final_parse_error) => {
                        let final_message = format!(
                            "parse failed after one repair retry: {final_parse_error}; raw_response={}",
                            truncate_for_log(&model_output.raw_response, 4000)
                        );

                        log_lab_generation_failure(
                            request_id,
                            generation_mode,
                            "parse",
                            model_output.model.as_str(),
                            Some(estimate.input_tokens),
                            model_output.usage.actual_input_tokens,
                            model_output.usage.actual_output_tokens,
                            generation_attempt,
                            base_lab_ref.clone(),
                            base_lab_files_count,
                            system_layer.chars().count(),
                            prompt_bundle.layer2_chars,
                            request_layer.chars().count(),
                            base_lab_chars,
                            Some(false),
                            None,
                            None,
                            &final_message,
                            &model_output.raw_response,
                            true,
                        );

                        return Err(ProcessFailure::new(
                            "OUTPUT_PARSE_FAILED",
                            final_message,
                            false,
                        ));
                    }
                }
            }
        };

        if let Err(validation_error) = validate_generated_lab(lab_type, difficulty, &files) {
            if generation_attempt >= LAB_GENERATION_REPAIR_ATTEMPT {
                let final_message = format!(
                    "validation failed after one repair retry: {validation_error}; raw_response={}",
                    truncate_for_log(&model_output.raw_response, 4000)
                );

                log_lab_generation_failure(
                    request_id,
                    generation_mode,
                    "validate",
                    model_output.model.as_str(),
                    Some(estimate.input_tokens),
                    model_output.usage.actual_input_tokens,
                    model_output.usage.actual_output_tokens,
                    generation_attempt,
                    base_lab_ref.clone(),
                    base_lab_files_count,
                    system_layer.chars().count(),
                    prompt_bundle.layer2_chars,
                    request_layer.chars().count(),
                    base_lab_chars,
                    Some(true),
                    Some(false),
                    Some(files.len()),
                    &final_message,
                    &model_output.raw_response,
                    true,
                );

                return Err(ProcessFailure::new(
                    "VALIDATION_FAILED",
                    final_message,
                    false,
                ));
            }

            user_payload = build_lab_repair_payload(
                &user_payload,
                "validation",
                &validation_error,
                &model_output.raw_response,
            );

            generation_attempt = LAB_GENERATION_REPAIR_ATTEMPT;
            log_lab_repair_dispatch(
                request_id,
                generation_mode,
                model_output.model.as_str(),
                Some(estimate.input_tokens),
                model_output.usage.actual_input_tokens,
                model_output.usage.actual_output_tokens,
                generation_attempt,
                base_lab_ref.clone(),
                base_lab_files_count,
                system_layer.chars().count(),
                prompt_bundle.layer2_chars,
                request_layer.chars().count(),
                base_lab_chars,
                Some(true),
                Some(false),
                Some(files.len()),
                &validation_error,
            );

            let repair_provider = model_output.provider.clone();
            model_output = self
                .llm
                .generate_lab_files_with_preferred_provider(
                    &LabGenerationInput {
                        run_id: request_id,
                        mode: mode.clone(),
                        system: system_layer.clone(),
                        user_message: user_payload.clone(),
                    },
                    repair_provider.as_str(),
                )
                .await
                .map_err(|e| map_llm_error("MODEL_CALL_FAILED", e))?;
            used_model_fallback |= model_output.used_fallback;

            if let Some(reason) = extract_unsatisfiable_reason(&model_output.raw_response) {
                return Err(ProcessFailure::new("UNSATISFIABLE_REQUEST", reason, false));
            }

            files = match parse_lab_files(&model_output.raw_response) {
                Ok(files) => {
                    log_stage(StageLog {
                        run_id: request_id,
                        mode: generation_mode,
                        stage: "parse",
                        model: model_output.model.as_str(),
                        estimated_input_tokens: Some(estimate.input_tokens),
                        actual_input_tokens: model_output.usage.actual_input_tokens,
                        actual_output_tokens: model_output.usage.actual_output_tokens,
                        attempt: generation_attempt,
                        base_lab_ref: base_lab_ref.clone(),
                        base_lab_files_count,
                        prompt_layers: PromptLayersMetrics {
                            system_chars: system_layer.chars().count(),
                            playbook_chars: prompt_bundle.layer2_chars,
                            request_chars: request_layer.chars().count(),
                            base_lab_chars,
                        },
                        parse_success: Some(true),
                        validation_success: None,
                        files_generated: Some(files.len()),
                        error: None,
                    });
                    files
                }
                Err(parse_error) => {
                    let final_message = format!(
                        "validation repair retry parse failed: {parse_error}; raw_response={}",
                        truncate_for_log(&model_output.raw_response, 4000)
                    );

                    log_lab_generation_failure(
                        request_id,
                        generation_mode,
                        "parse",
                        model_output.model.as_str(),
                        Some(estimate.input_tokens),
                        model_output.usage.actual_input_tokens,
                        model_output.usage.actual_output_tokens,
                        generation_attempt,
                        base_lab_ref.clone(),
                        base_lab_files_count,
                        system_layer.chars().count(),
                        prompt_bundle.layer2_chars,
                        request_layer.chars().count(),
                        base_lab_chars,
                        Some(false),
                        None,
                        None,
                        &final_message,
                        &model_output.raw_response,
                        true,
                    );

                    return Err(ProcessFailure::new(
                        "OUTPUT_PARSE_FAILED",
                        final_message,
                        false,
                    ));
                }
            };

            if let Err(final_error) = validate_generated_lab(lab_type, difficulty, &files) {
                let final_message = format!(
                    "validation failed after one repair retry: {final_error}; raw_response={}",
                    truncate_for_log(&model_output.raw_response, 4000)
                );

                log_lab_generation_failure(
                    request_id,
                    generation_mode,
                    "validate",
                    model_output.model.as_str(),
                    Some(estimate.input_tokens),
                    model_output.usage.actual_input_tokens,
                    model_output.usage.actual_output_tokens,
                    generation_attempt,
                    base_lab_ref.clone(),
                    base_lab_files_count,
                    system_layer.chars().count(),
                    prompt_bundle.layer2_chars,
                    request_layer.chars().count(),
                    base_lab_chars,
                    Some(true),
                    Some(false),
                    Some(files.len()),
                    &final_message,
                    &model_output.raw_response,
                    true,
                );

                return Err(ProcessFailure::new(
                    "VALIDATION_FAILED",
                    final_message,
                    false,
                ));
            }
        }

        log_stage(StageLog {
            run_id: request_id,
            mode: generation_mode,
            stage: "validate",
            model: model_output.model.as_str(),
            estimated_input_tokens: Some(estimate.input_tokens),
            actual_input_tokens: model_output.usage.actual_input_tokens,
            actual_output_tokens: model_output.usage.actual_output_tokens,
            attempt: generation_attempt,
            base_lab_ref: base_lab_ref.clone(),
            base_lab_files_count,
            prompt_layers: PromptLayersMetrics {
                system_chars: system_layer.chars().count(),
                playbook_chars: prompt_bundle.layer2_chars,
                request_chars: request_layer.chars().count(),
                base_lab_chars,
            },
            parse_success: Some(true),
            validation_success: Some(true),
            files_generated: Some(files.len()),
            error: None,
        });

        let zip_bytes = build_result_zip(&files).map_err(|error| {
            ProcessFailure::new(
                "RESULT_BUILD_FAILED",
                format!("failed to build result zip: {error}"),
                false,
            )
        })?;

        let result_object_key = self.storage.result_object_key(request_id);
        self.storage
            .upload_result_bytes(&result_object_key, zip_bytes, "application/zip")
            .await
            .map_err(|error| {
                ProcessFailure::new(
                    "RESULT_UPLOAD_FAILED",
                    format!("failed to upload result artifact: {error}"),
                    true,
                )
            })?;

        if let Err(error) = self
            .runs_repo
            .update_observability(
                request_id,
                Some(estimate.input_tokens),
                model_output.usage.actual_input_tokens,
                model_output.usage.actual_output_tokens,
                &excluded_files,
            )
            .await
        {
            tracing::warn!(run_id = %request_id, error = %error, "failed to persist observability fields");
        }

        log_stage(StageLog {
            run_id: request_id,
            mode: generation_mode,
            stage: "zip",
            model: model_output.model.as_str(),
            estimated_input_tokens: Some(estimate.input_tokens),
            actual_input_tokens: model_output.usage.actual_input_tokens,
            actual_output_tokens: model_output.usage.actual_output_tokens,
            attempt: generation_attempt,
            base_lab_ref: base_lab_ref.clone(),
            base_lab_files_count,
            prompt_layers: PromptLayersMetrics {
                system_chars: system_layer.chars().count(),
                playbook_chars: prompt_bundle.layer2_chars,
                request_chars: request_layer.chars().count(),
                base_lab_chars,
            },
            parse_success: Some(true),
            validation_success: Some(true),
            files_generated: Some(files.len()),
            error: None,
        });

        Ok(ProcessSuccess {
            mode,
            result_object_key,
            used_model_fallback,
        })
    }

    async fn load_source_objects_as_base_lab_files(
        &self,
        source_objects: &[String],
    ) -> ProcessResult<Vec<BaseLabFile>> {
        let mut by_path: BTreeMap<String, BaseLabFile> = BTreeMap::new();

        for object_key in source_objects {
            let bytes = self
                .storage
                .download_object_bytes(object_key)
                .await
                .map_err(|error| {
                    ProcessFailure::new(
                        "SOURCE_DOWNLOAD_FAILED",
                        format!("failed to download source object `{object_key}`: {error}"),
                        true,
                    )
                })?;

            if object_key.to_ascii_lowercase().ends_with(".zip") {
                let files = parse_zip_to_base_lab_files(&bytes).map_err(|error| {
                    ProcessFailure::new(
                        "SOURCE_PARSE_FAILED",
                        format!("failed to parse source zip `{object_key}`: {error}"),
                        false,
                    )
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
            return Err(ProcessFailure::new(
                "SOURCE_PARSE_FAILED",
                "no usable files extracted from source_objects",
                false,
            ));
        }

        Ok(by_path.into_values().collect::<Vec<_>>())
    }
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
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("source-file.txt")
        .to_string()
}

fn build_lab_repair_payload(
    original_payload: &str,
    failure_kind: &str,
    failure_message: &str,
    raw_response: &str,
) -> String {
    format!(
        "{original_payload}\n\n<repair_context>\nPrevious lab generation failed during {failure_kind}.\nReason: {failure_message}\n\nRegenerate the complete lab exactly once. Return the full corrected output, not a diff. Keep the required <<<FILE: path>>> ... <<<END>>> format for every file.\n\nPrevious raw response excerpt:\n{}\n</repair_context>",
        truncate_for_log(raw_response, 2000)
    )
}

#[allow(clippy::too_many_arguments)]
fn log_lab_repair_dispatch(
    run_id: Uuid,
    mode: GenerationMode,
    model: &str,
    estimated_input_tokens: Option<u64>,
    actual_input_tokens: Option<u64>,
    actual_output_tokens: Option<u64>,
    attempt: u8,
    base_lab_ref: Option<String>,
    base_lab_files_count: usize,
    system_chars: usize,
    playbook_chars: usize,
    request_chars: usize,
    base_lab_chars: usize,
    parse_success: Option<bool>,
    validation_success: Option<bool>,
    files_generated: Option<usize>,
    error: &str,
) {
    tracing::warn!(
        run_id = %run_id,
        attempt,
        max_repair_retries = LAB_GENERATION_REPAIR_RETRIES,
        error = %error,
        "lab generation failed validation/parsing, sending repair context to LLM"
    );

    log_stage(StageLog {
        run_id,
        mode,
        stage: "api_call",
        model,
        estimated_input_tokens,
        actual_input_tokens,
        actual_output_tokens,
        attempt,
        base_lab_ref,
        base_lab_files_count,
        prompt_layers: PromptLayersMetrics {
            system_chars,
            playbook_chars,
            request_chars,
            base_lab_chars,
        },
        parse_success,
        validation_success,
        files_generated,
        error: Some(error.to_string()),
    });
}

#[allow(clippy::too_many_arguments)]
fn log_lab_generation_failure(
    run_id: Uuid,
    mode: GenerationMode,
    stage: &'static str,
    model: &str,
    estimated_input_tokens: Option<u64>,
    actual_input_tokens: Option<u64>,
    actual_output_tokens: Option<u64>,
    attempt: u8,
    base_lab_ref: Option<String>,
    base_lab_files_count: usize,
    system_chars: usize,
    playbook_chars: usize,
    request_chars: usize,
    base_lab_chars: usize,
    parse_success: Option<bool>,
    validation_success: Option<bool>,
    files_generated: Option<usize>,
    error: &str,
    raw_response: &str,
    final_failure: bool,
) {
    if final_failure {
        tracing::error!(
            run_id = %run_id,
            stage,
            attempt,
            error = %error,
            raw_response = %truncate_for_log(raw_response, 4000),
            "lab generation failed after one repair retry"
        );
    } else {
        tracing::warn!(
            run_id = %run_id,
            stage,
            attempt,
            error = %error,
            raw_response = %truncate_for_log(raw_response, 1000),
            "lab generation failed before repair retry"
        );
    }

    log_stage(StageLog {
        run_id,
        mode,
        stage,
        model,
        estimated_input_tokens,
        actual_input_tokens,
        actual_output_tokens,
        attempt,
        base_lab_ref,
        base_lab_files_count,
        prompt_layers: PromptLayersMetrics {
            system_chars,
            playbook_chars,
            request_chars,
            base_lab_chars,
        },
        parse_success,
        validation_success,
        files_generated,
        error: Some(error.to_string()),
    });
}

fn map_llm_error(default_code: &'static str, error: LlmError) -> ProcessFailure {
    match error.kind {
        LlmErrorKind::PromptTooLarge => {
            ProcessFailure::new("PROMPT_TOO_LARGE", error.message, false)
        }
        LlmErrorKind::ModelUnavailable => {
            ProcessFailure::new("MODEL_UNAVAILABLE", error.message, false)
        }
        LlmErrorKind::InvalidRequest => {
            ProcessFailure::new("INVALID_REQUEST", error.message, false)
        }
        LlmErrorKind::Unauthorized | LlmErrorKind::Forbidden => {
            ProcessFailure::new("MODEL_AUTH_FAILED", error.message, false)
        }
        LlmErrorKind::Unprocessable => ProcessFailure::new("INVALID_REQUEST", error.message, false),
        LlmErrorKind::RateLimited | LlmErrorKind::ServerError | LlmErrorKind::Transport => {
            ProcessFailure::new(default_code, error.message, true)
        }
        LlmErrorKind::Decode | LlmErrorKind::EmptyResponse => {
            ProcessFailure::new("OUTPUT_PARSE_FAILED", error.message, false)
        }
        LlmErrorKind::TemporarilyUnavailable => {
            ProcessFailure::new("AI_TEMPORARILY_UNAVAILABLE", error.message, false)
        }
    }
}

fn parse_lab_type_from_request(request: &str) -> Option<LabType> {
    let value = extract_xml_tag_value(request, "lab_type")?;
    LabType::parse(&value)
}

fn parse_difficulty_from_request(request: &str) -> Option<Difficulty> {
    let value = extract_xml_tag_value(request, "difficulty")?;
    Difficulty::parse(&value)
}

fn extract_xml_tag_value(input: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = input.find(&open)? + open.len();
    let end = input[start..].find(&close)? + start;
    Some(input[start..end].trim().to_string())
}

fn fallback_request_layer(prompt: &str, lab_type: LabType, mode: GenerationMode) -> String {
    let mode_value = match mode {
        GenerationMode::Initial => "initial",
        GenerationMode::Variant => "variant",
    };
    let lab_type_value = match lab_type {
        LabType::Web => "web",
        LabType::Terminal => "terminal",
    };

    format!(
        "<lab_request>\n  <mode>{}</mode>\n  <lab_type>{}</lab_type>\n  <difficulty>course</difficulty>\n  <stack_main>unspecified</stack_main>\n  <goal>{}</goal>\n  <functional_description>{}</functional_description>\n  <security_description>{}</security_description>\n  <frameworks>[]</frameworks>\n  <forced_dependencies>[]</forced_dependencies>\n  <visual_description></visual_description>\n  <hard_constraints>ctf_form_v1 fallback mapping</hard_constraints>\n</lab_request>",
        mode_value,
        lab_type_value,
        xml_escape(prompt),
        xml_escape(prompt),
        xml_escape(prompt)
    )
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn extract_requeue_cycle(message: Option<&str>) -> u8 {
    let Some(raw) = message else {
        return 0;
    };
    let marker = "requeue_cycle=";
    let Some(pos) = raw.find(marker) else {
        return 0;
    };
    let tail = &raw[pos + marker.len()..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse::<u8>().unwrap_or(0)
}

fn log_stage(event: StageLog<'_>) {
    if let Ok(as_json) = serde_json::to_string(&event) {
        tracing::info!("{}", as_json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requeue_cycle_parse() {
        let raw = "requeue_cycle=2; temporary outage";
        assert_eq!(extract_requeue_cycle(Some(raw)), 2);
        assert_eq!(extract_requeue_cycle(None), 0);
    }
}
