use std::io::{Error, ErrorKind};

#[derive(Debug, Clone, Copy)]
pub enum PromptLabType {
    Web,
    Terminal,
}

pub struct PromptBundle {
    pub system: String,
    pub layer2_chars: usize,
}

pub struct QualificationPromptBundle {
    pub system: String,
    pub playbook_chars: usize,
}

const LAYER1_BASE_SYSTEM_PATH: &str = "system-prompts/ctf-generation/layer1_base_system.txt";
const LAYER2_FORM_FIELD_SEMANTICS_PATH: &str =
    "system-prompts/ctf-generation/layer2_form_field_semantics.txt";
const LAYER3_OUTPUT_CONTRACT_PATH: &str =
    "system-prompts/ctf-generation/layer3_output_contract.txt";
const VARIANT_PLAYBOOK_APPEND_PATH: &str =
    "system-prompts/ctf-generation/variant_playbook_append.txt";

const PLAYBOOK_WEB_PRIMARY_PATH: &str = "system-prompts/ctf-generation/playbooks/web_v1.txt";
const PLAYBOOK_TERMINAL_PRIMARY_PATH: &str =
    "system-prompts/ctf-generation/playbooks/terminal_v1.txt";
const PLAYBOOK_WEB_LEGACY_PATH: &str = "playbooks/web_v1.txt";
const PLAYBOOK_TERMINAL_LEGACY_PATH: &str = "playbooks/terminal_v1.txt";
const QUALIFICATION_WEB_PATH: &str =
    "system-prompts/ctf-generation/qualification/qualification_web_v1.txt";
const QUALIFICATION_TERMINAL_PATH: &str =
    "system-prompts/ctf-generation/qualification/qualification_terminal_v1.txt";
const QUALIFICATION_OUTPUT_CONTRACT_PATH: &str =
    "system-prompts/ctf-generation/qualification/qualification_output_contract_v1.txt";

pub fn build_system_prompt(
    lab_type: PromptLabType,
    include_variant_rules: bool,
) -> std::io::Result<PromptBundle> {
    let layer1 = load_required_asset(LAYER1_BASE_SYSTEM_PATH)?;
    let layer2 = load_required_asset(LAYER2_FORM_FIELD_SEMANTICS_PATH)?;
    let layer3 = load_required_asset(LAYER3_OUTPUT_CONTRACT_PATH)?;
    let playbook = load_playbook(lab_type)?;

    let playbook_layer = if include_variant_rules {
        let variant_rules = load_required_asset(VARIANT_PLAYBOOK_APPEND_PATH)?;
        format!("{}\n\n{}", playbook, variant_rules)
    } else {
        playbook
    };

    let layer2_block = format!("{}\n\n{}", layer2, playbook_layer);
    let system = format!("{}\n\n{}\n\n{}", layer1, layer2_block, layer3);

    Ok(PromptBundle {
        system,
        layer2_chars: layer2_block.chars().count(),
    })
}

pub fn build_qualification_prompt(
    lab_type: PromptLabType,
) -> std::io::Result<QualificationPromptBundle> {
    let playbook = load_qualification_playbook(lab_type)?;
    let output_contract = load_required_asset(QUALIFICATION_OUTPUT_CONTRACT_PATH)?;
    let system = format!("{}\n\n{}", playbook, output_contract);

    Ok(QualificationPromptBundle {
        playbook_chars: playbook.chars().count(),
        system,
    })
}

fn load_playbook(lab_type: PromptLabType) -> std::io::Result<String> {
    let relatives = match lab_type {
        // Primary path is the new dedicated prompt directory.
        // Legacy path keeps backward compatibility while we migrate.
        PromptLabType::Web => &[PLAYBOOK_WEB_PRIMARY_PATH, PLAYBOOK_WEB_LEGACY_PATH][..],
        PromptLabType::Terminal => &[
            PLAYBOOK_TERMINAL_PRIMARY_PATH,
            PLAYBOOK_TERMINAL_LEGACY_PATH,
        ][..],
    };

    load_first_existing_asset(relatives)
}

fn load_qualification_playbook(lab_type: PromptLabType) -> std::io::Result<String> {
    let relative = match lab_type {
        PromptLabType::Web => QUALIFICATION_WEB_PATH,
        PromptLabType::Terminal => QUALIFICATION_TERMINAL_PATH,
    };

    load_required_asset(relative)
}

fn load_required_asset(relative: &str) -> std::io::Result<String> {
    load_first_existing_asset(&[relative])
}

fn load_first_existing_asset(relatives: &[&str]) -> std::io::Result<String> {
    let mut attempted = Vec::new();
    for base in candidate_roots() {
        for relative in relatives {
            let full = base.join(relative);
            attempted.push(full.display().to_string());
            if !full.exists() {
                continue;
            }

            return std::fs::read_to_string(&full).map_err(|read_error| {
                Error::new(
                    read_error.kind(),
                    format!(
                        "failed to read prompt asset {}: {}",
                        full.display(),
                        read_error
                    ),
                )
            });
        }
    }

    Err(Error::new(
        ErrorKind::NotFound,
        format!(
            "prompt asset not found; attempted: {}",
            attempted.join(" | ")
        ),
    ))
}

fn candidate_roots() -> [std::path::PathBuf; 2] {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [cwd, manifest]
}
