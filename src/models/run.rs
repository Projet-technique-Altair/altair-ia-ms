use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    GenerateFromScratch,
    CreateVariant,
}

impl RunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GenerateFromScratch => "generate_from_scratch",
            Self::CreateVariant => "create_variant",
        }
    }

    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "generate_from_scratch" => Some(Self::GenerateFromScratch),
            "create_variant" => Some(Self::CreateVariant),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IaRun {
    pub request_id: Uuid,
    pub creator_id: String,
    pub prompt: String,
    pub status: RunStatus,
    pub mode_selected: Option<String>,
    pub input_refs: Vec<String>,
    pub result_object_key: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub used_model_fallback: bool,
    pub estimated_input_tokens: Option<u64>,
    pub actual_input_tokens: Option<u64>,
    pub actual_output_tokens: Option<u64>,
    pub excluded_source_objects: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl IaRun {
    pub fn new(
        request_id: Uuid,
        creator_id: String,
        prompt: String,
        mode_selected: Option<String>,
        input_refs: Vec<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            request_id,
            creator_id,
            prompt,
            status: RunStatus::Queued,
            mode_selected,
            input_refs,
            result_object_key: None,
            error_code: None,
            error_message: None,
            used_model_fallback: false,
            estimated_input_tokens: None,
            actual_input_tokens: None,
            actual_output_tokens: None,
            excluded_source_objects: Vec::new(),
            created_at: now,
            updated_at: now,
            finished_at: None,
        }
    }
}
