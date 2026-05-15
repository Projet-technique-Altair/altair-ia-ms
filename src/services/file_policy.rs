const ALLOWED_EXTENSIONS: [&str; 9] = [".txt", ".md", ".json", ".js", ".html", ".css", ".sh", ".c", ".py"];

const BLOCKED_EXTENSIONS: [&str; 19] = [
    "exe", "dll", "so", "dylib", "bat", "cmd", "ps1", "msi", "bin", "elf", "jar", "class", "pyc",
    "pyo", "zip", "tar", "gz", "tgz", "rar",
];

const EXACT_TEXT_FILENAMES: [&str; 3] = ["Dockerfile", "Makefile", "README"];

pub fn is_allowed_upload_name(name: &str) -> bool {
    is_text_path(name)
}

pub fn is_text_path(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    if normalized.is_empty() {
        return false;
    }

    let filename = normalized.rsplit('/').next().unwrap_or(&normalized);
    if filename.is_empty() {
        return false;
    }

    if EXACT_TEXT_FILENAMES.contains(&filename) {
        return true;
    }

    let extension_chain = filename.split('.').skip(1).collect::<Vec<_>>();
    if extension_chain.is_empty() {
        return false;
    }

    if extension_chain.iter().any(|extension| {
        let lower = extension.to_ascii_lowercase();
        BLOCKED_EXTENSIONS.contains(&lower.as_str())
    }) {
        return false;
    }

    let lower = filename.to_ascii_lowercase();
    ALLOWED_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_upload_name, is_text_path};

    #[test]
    fn upload_policy_accepts_dockerfile() {
        assert!(is_allowed_upload_name("Dockerfile"));
        assert!(is_text_path("uploads/r1/Dockerfile"));
        assert!(is_allowed_upload_name("README.md"));
        assert!(is_allowed_upload_name("assets/app.min.js"));
        assert!(is_allowed_upload_name("src/runner.c"));
        assert!(is_allowed_upload_name("src/script.py"));
    }

    #[test]
    fn blocks_non_supported_binary_extensions() {
        assert!(!is_allowed_upload_name("payload.exe"));
        assert!(!is_allowed_upload_name("runner"));
        assert!(!is_allowed_upload_name("server.pyc"));
        assert!(!is_allowed_upload_name("source.zip"));
        assert!(!is_allowed_upload_name("source.tar.gz"));
        assert!(!is_allowed_upload_name("script.sh.exe"));
        assert!(!is_allowed_upload_name("payload.exe.txt"));
        assert!(!is_allowed_upload_name("lib.so.js"));
        assert!(!is_allowed_upload_name("index.html.bak"));
    }
}
