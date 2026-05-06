use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabType {
    Web,
    Terminal,
}

impl LabType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Terminal => "terminal",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "web" => Some(Self::Web),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Difficulty {
    Course,
    Guided,
    NonGuided,
}

impl Difficulty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Course => "course",
            Self::Guided => "guided",
            Self::NonGuided => "non guided",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "course" => Some(Self::Course),
            "guided" => Some(Self::Guided),
            "non guided" | "non_guided" => Some(Self::NonGuided),
            _ => None,
        }
    }
}
