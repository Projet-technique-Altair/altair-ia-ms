const TEXT_EXTENSIONS: [&str; 25] = [
    ".txt",
    ".md",
    ".json",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".csv",
    ".sql",
    ".py",
    ".js",
    ".ts",
    ".tsx",
    ".jsx",
    ".html",
    ".css",
    ".rs",
    ".go",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".sh",
    ".dockerfile",
];

const EXACT_TEXT_FILENAMES: [&str; 4] = ["Dockerfile", "Makefile", "README", "README.md"];

pub fn is_allowed_upload_name(name: &str) -> bool {
    if name.ends_with(".zip") {
        return true;
    }

    if is_text_path(name) {
        return true;
    }

    false
}

pub fn is_text_path(path: &str) -> bool {
    if EXACT_TEXT_FILENAMES.contains(&path) {
        return true;
    }

    let filename = path.rsplit('/').next().unwrap_or(path);
    if EXACT_TEXT_FILENAMES.contains(&filename) {
        return true;
    }

    let lower = path.to_ascii_lowercase();
    TEXT_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_upload_name, is_text_path};

    #[test]
    fn upload_policy_accepts_dockerfile() {
        assert!(is_allowed_upload_name("Dockerfile"));
        assert!(is_text_path("uploads/r1/Dockerfile"));
    }

    #[test]
    fn blocks_non_supported_binary_extensions() {
        assert!(!is_allowed_upload_name("payload.exe"));
    }
}
