use std::{
    io::{Read, Write},
    sync::OnceLock,
};

use regex::Regex;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{models::lab::Difficulty, models::lab::LabType, services::llm::estimate_tokens};

#[derive(Debug, Clone)]
pub(crate) struct ParsedLabFile {
    pub(crate) path: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone)]
pub(crate) struct BaseLabFile {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) estimated_tokens: u64,
}

pub(crate) fn parse_lab_files(raw: &str) -> anyhow::Result<Vec<ParsedLabFile>> {
    static FILE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    let regex = FILE_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?s)<<<FILE:\s*(.+?)>>>\n(.*?)<<<END>>>")
            .expect("file delimiter regex must compile")
    });

    let matches = regex
        .captures_iter(raw)
        .map(|capture| ParsedLabFile {
            path: capture
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            content: capture
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    if matches.is_empty() {
        anyhow::bail!("No file blocks found in response")
    }

    Ok(matches)
}

pub(crate) fn validate_generated_lab(
    lab_type: LabType,
    difficulty: Option<Difficulty>,
    files: &[ParsedLabFile],
) -> Result<(), String> {
    match lab_type {
        LabType::Web => validate_web_lab(files)?,
        LabType::Terminal => validate_terminal_lab(files)?,
    }
    validate_difficulty_rules(difficulty, files)?;
    Ok(())
}

pub(crate) fn extract_unsatisfiable_reason(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("ERROR:") {
        return None;
    }
    let reason = trimmed.trim_start_matches("ERROR:").trim();
    if reason.is_empty() {
        return Some("request is unsatisfiable".to_string());
    }
    Some(reason.to_string())
}

pub(crate) fn build_result_zip(files: &[ParsedLabFile]) -> anyhow::Result<Vec<u8>> {
    let mut output = std::io::Cursor::new(Vec::<u8>::new());
    {
        let mut zip = ZipWriter::new(&mut output);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        for file in files {
            let path = normalize_tree_path(&file.path)?;
            zip.start_file(path, opts)?;
            zip.write_all(file.content.as_bytes())?;
        }

        zip.finish()?;
    }

    Ok(output.into_inner())
}

pub(crate) fn parse_zip_to_base_lab_files(bytes: &[u8]) -> anyhow::Result<Vec<BaseLabFile>> {
    let mut archive = ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut files = Vec::new();

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }

        if is_symlink_mode(file.unix_mode()) {
            anyhow::bail!("zip contains symlink entry: {}", file.name());
        }

        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("zip contains unsafe path: {}", file.name()))?;
        let path = normalize_tree_path(&enclosed.to_string_lossy())?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        let content = match String::from_utf8(buf) {
            Ok(text) => text,
            Err(error) => format!(
                "[binary file omitted: {} bytes, path={}]",
                error.as_bytes().len(),
                path
            ),
        };

        let estimated_tokens = estimate_tokens(&content);
        files.push(BaseLabFile {
            path,
            content,
            estimated_tokens,
        });
    }

    Ok(files)
}

pub(crate) fn truncate_base_lab_files(
    files: Vec<BaseLabFile>,
    warning_threshold: u64,
) -> (Vec<BaseLabFile>, Vec<String>) {
    let mut always_include = files
        .iter()
        .filter(|f| is_core_base_file(&f.path))
        .cloned()
        .collect::<Vec<_>>();

    always_include.sort_by(|a, b| a.path.cmp(&b.path));

    let always_paths = always_include
        .iter()
        .map(|f| f.path.clone())
        .collect::<std::collections::BTreeSet<_>>();

    let mut candidates = files
        .into_iter()
        .filter(|f| !always_paths.contains(&f.path))
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| {
        relevance_score(&b.path)
            .cmp(&relevance_score(&a.path))
            .then_with(|| b.estimated_tokens.cmp(&a.estimated_tokens))
    });

    let mut selected = always_include;
    let mut current_tokens = selected.iter().map(|f| f.estimated_tokens).sum::<u64>();

    let mut excluded = Vec::new();
    for file in candidates {
        let would_fit = current_tokens.saturating_add(file.estimated_tokens) <= warning_threshold;
        if would_fit || current_tokens == 0 {
            current_tokens = current_tokens.saturating_add(file.estimated_tokens);
            selected.push(file);
        } else {
            excluded.push(file.path);
        }
    }

    if current_tokens > warning_threshold {
        tracing::warn!(
            base_lab_tokens = current_tokens,
            threshold = warning_threshold,
            "base lab tokens exceed warning threshold after truncation"
        );
    }

    selected.sort_by(|a, b| a.path.cmp(&b.path));
    (selected, excluded)
}

pub(crate) fn render_base_lab_block(base_lab_ref: &str, files: &[BaseLabFile]) -> String {
    let mut out = String::new();
    out.push_str(&format!("<base_lab run_id=\"{}\">\n", base_lab_ref));
    for file in files {
        out.push_str(&format!("<<<FILE: {}>>>\n", file.path));
        out.push_str(&file.content);
        if !file.content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("<<<END>>>\n");
    }
    out.push_str("</base_lab>");
    out
}

pub(crate) fn truncate_for_log(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    format!("{}...[truncated]", &value[..max_len])
}

fn validate_difficulty_rules(
    difficulty: Option<Difficulty>,
    files: &[ParsedLabFile],
) -> Result<(), String> {
    let has_hints = files
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case("HINTS.md"));
    match difficulty {
        Some(Difficulty::Guided) if !has_hints => {
            Err("guided difficulty requires HINTS.md".to_string())
        }
        Some(Difficulty::NonGuided) if has_hints => {
            Err("non guided difficulty forbids HINTS.md".to_string())
        }
        _ => Ok(()),
    }
}

fn validate_web_lab(files: &[ParsedLabFile]) -> Result<(), String> {
    let paths = files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>();
    if !paths.contains(&"Dockerfile") {
        return Err("Missing Dockerfile".to_string());
    }

    let dockerfile_content = files
        .iter()
        .find(|f| f.path == "Dockerfile")
        .map(|f| f.content.as_str())
        .ok_or_else(|| "Missing Dockerfile".to_string())?;

    if !dockerfile_content.contains("EXPOSE") {
        return Err("Dockerfile missing EXPOSE instruction".to_string());
    }
    if !dockerfile_content.contains("CMD") && !dockerfile_content.contains("ENTRYPOINT") {
        return Err("Dockerfile missing CMD or ENTRYPOINT".to_string());
    }

    let flag_count = count_ctf_flags(files);
    if flag_count < 1 {
        return Err("No CTF{...} flag found in any file".to_string());
    }
    if files.len() < 2 {
        return Err("Lab has fewer than 2 files, likely incomplete".to_string());
    }
    Ok(())
}

fn validate_terminal_lab(files: &[ParsedLabFile]) -> Result<(), String> {
    let paths = files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>();
    let has_entrypoint = ["Makefile", "run.sh", "start.sh", "entrypoint.sh"]
        .iter()
        .any(|p| paths.contains(p));
    if !has_entrypoint {
        return Err("Missing entrypoint (Makefile or shell script)".to_string());
    }

    let flag_count = count_ctf_flags(files);
    if flag_count < 1 {
        return Err("No CTF{...} flag found in any file".to_string());
    }
    Ok(())
}

fn count_ctf_flags(files: &[ParsedLabFile]) -> usize {
    static CTF_FLAG_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        CTF_FLAG_RE.get_or_init(|| Regex::new(r"CTF\{[^}\n]+\}").expect("ctf regex must compile"));
    files
        .iter()
        .map(|f| regex.find_iter(&f.content).count())
        .sum::<usize>()
}

fn is_core_base_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        file_name,
        "Dockerfile" | "entrypoint.sh" | "run.sh" | "start.sh" | "Makefile"
    )
}

fn relevance_score(path: &str) -> i32 {
    let p = path.to_ascii_lowercase();
    if p == "dockerfile" || p.ends_with("/dockerfile") {
        return 1000;
    }
    if ["entrypoint.sh", "run.sh", "start.sh", "makefile"]
        .iter()
        .any(|k| p.ends_with(k))
    {
        return 900;
    }
    if p.starts_with("src/") {
        return 700;
    }
    if p.ends_with(".js") || p.ends_with(".ts") || p.ends_with(".py") || p.ends_with(".rs") {
        return 600;
    }
    if p.ends_with(".md") || p.ends_with(".json") || p.ends_with(".toml") {
        return 400;
    }
    100
}

fn is_symlink_mode(mode: Option<u32>) -> bool {
    mode.map(|m| (m & 0o170000) == 0o120000).unwrap_or(false)
}

fn normalize_tree_path(path: &str) -> anyhow::Result<String> {
    let raw = path.trim().replace('\\', "/");
    if raw.is_empty() {
        anyhow::bail!("empty path");
    }
    if raw.starts_with('/') {
        anyhow::bail!("absolute path not allowed");
    }
    if raw.contains("../") || raw.contains("/..") || raw == ".." {
        anyhow::bail!("path traversal detected");
    }
    if raw.contains('\0') {
        anyhow::bail!("null byte not allowed");
    }

    let mut clean_segments = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            anyhow::bail!("path traversal segment");
        }
        clean_segments.push(segment.to_string());
    }

    if clean_segments.is_empty() {
        anyhow::bail!("path normalized to empty");
    }

    Ok(clean_segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_blocks() {
        let raw = "<<<FILE: Dockerfile>>>\nFROM node:20\n<<<END>>>\n<<<FILE: app.js>>>\nconsole.log('x')\n<<<END>>>";
        let parsed = parse_lab_files(raw).expect("must parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "Dockerfile");
    }

    #[test]
    fn validate_web_checks_dockerfile() {
        let files = vec![
            ParsedLabFile {
                path: "Dockerfile".to_string(),
                content: "FROM node:20\nEXPOSE 3000\nCMD [\"node\",\"server.js\"]\n".to_string(),
            },
            ParsedLabFile {
                path: "server.js".to_string(),
                content: "const trainingFlag='CTF{demo_flag}';".to_string(),
            },
        ];
        assert!(validate_generated_lab(LabType::Web, Some(Difficulty::Course), &files).is_ok());
    }

    #[test]
    fn detects_unsatisfiable_error_contract() {
        let raw = "ERROR: impossible to satisfy constraints";
        let out = extract_unsatisfiable_reason(raw).expect("must detect error contract");
        assert_eq!(out, "impossible to satisfy constraints");
    }

    #[test]
    fn guided_requires_hints_md() {
        let files = vec![
            ParsedLabFile {
                path: "Dockerfile".to_string(),
                content: "FROM node:20\nEXPOSE 3000\nCMD [\"node\",\"server.js\"]\n".to_string(),
            },
            ParsedLabFile {
                path: "server.js".to_string(),
                content: "const flag='CTF{demo_flag}';".to_string(),
            },
        ];
        let err = validate_generated_lab(LabType::Web, Some(Difficulty::Guided), &files)
            .expect_err("guided should fail without HINTS.md");
        assert!(err.contains("HINTS.md"));
    }

    #[test]
    fn non_guided_forbids_hints_md() {
        let files = vec![
            ParsedLabFile {
                path: "Dockerfile".to_string(),
                content: "FROM node:20\nEXPOSE 3000\nCMD [\"node\",\"server.js\"]\n".to_string(),
            },
            ParsedLabFile {
                path: "HINTS.md".to_string(),
                content: "indice".to_string(),
            },
            ParsedLabFile {
                path: "server.js".to_string(),
                content: "const flag='CTF{demo_flag}';".to_string(),
            },
        ];
        let err = validate_generated_lab(LabType::Web, Some(Difficulty::NonGuided), &files)
            .expect_err("non guided should fail with HINTS.md");
        assert!(err.contains("forbids"));
    }
}
