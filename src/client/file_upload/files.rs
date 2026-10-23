use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::config::Limits;

pub struct PreparedFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub snapshot: NamedTempFile,
}

pub fn prepare(paths: &[PathBuf], limits: &Limits) -> Result<Vec<PreparedFile>> {
    ensure!(!paths.is_empty(), "select at least one file");
    ensure!(
        paths.len() <= limits.max_files,
        "too many files (maximum {})",
        limits.max_files
    );
    let mut opened = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    // Validate the entire selection before copying or starting any network upload.
    for path in paths {
        let name = validate_name(path, limits)?;
        let file = crate::platform::open_upload_source(path, limits.allow_symlinks).with_context(
            || {
                format!(
                    "open {}; symlinks are rejected unless enabled",
                    path.display()
                )
            },
        )?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "{} is not a regular file",
            path.display()
        );
        let size = metadata.len();
        ensure!(
            size <= limits.max_file_bytes,
            "{name}: {size} bytes exceeds the {} byte limit",
            limits.max_file_bytes
        );
        total = total.checked_add(size).context("total size overflow")?;
        ensure!(
            total <= limits.max_total_bytes,
            "selection exceeds the {} byte total limit",
            limits.max_total_bytes
        );
        opened.push((name, size, file));
    }
    opened
        .into_iter()
        .map(|(name, size, file)| snapshot(name, size, file))
        .collect()
}

fn snapshot(name: String, size: u64, file: File) -> Result<PreparedFile> {
    let mut source = file.take(size.saturating_add(1));
    let mut snapshot = NamedTempFile::new()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut copied = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        ensure!(
            copied <= size,
            "{name} changed size during preparation; try again"
        );
        snapshot.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    ensure!(
        copied == size,
        "{name} changed size during preparation; try again"
    );
    snapshot.flush()?;
    Ok(PreparedFile {
        name,
        size,
        sha256: format!("{:x}", hasher.finalize()),
        snapshot,
    })
}

pub fn validate_name(path: &Path, limits: &Limits) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .context("filename must be valid UTF-8")?;
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control) && !name.contains('\\'),
        "unsupported filename"
    );
    ensure!(
        limits.allow_hidden || !name.starts_with('.'),
        "hidden files are disabled: {name}"
    );
    let extension = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    ensure!(
        !limits
            .deny_extensions
            .iter()
            .any(|v| v.eq_ignore_ascii_case(extension)),
        "file extension is denied: {name}"
    );
    ensure!(
        limits.allow_extensions.is_empty()
            || limits
                .allow_extensions
                .iter()
                .any(|v| v.eq_ignore_ascii_case(extension)),
        "file extension is not allowed: {name}"
    );
    Ok(name.into())
}

/// Decode file-manager URI lists or shell-quoted paths without executing a shell.
pub fn parse_paths(text: &str) -> Result<Vec<PathBuf>> {
    if text.lines().any(|line| line.starts_with("file://")) {
        return text
            .lines()
            .filter(|line| {
                !line.is_empty() && !line.starts_with('#') && *line != "copy" && *line != "cut"
            })
            .map(|line| {
                let url = url::Url::parse(line).context("invalid clipboard file URI")?;
                ensure!(
                    url.scheme() == "file"
                        && matches!(url.host_str(), None | Some("" | "localhost")),
                    "only local file URIs are supported"
                );
                url.to_file_path()
                    .map_err(|()| anyhow::anyhow!("invalid local file URI"))
            })
            .collect();
    }
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    // Windows backslashes need one-character lookahead to distinguish path separators.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            #[cfg(windows)]
            if !matches!(chars.peek(), Some(next) if next.is_whitespace() || *next == '\'' || *next == '"')
            {
                // Native Windows paths use backslashes as separators, not shell escapes.
                current.push('\\');
                continue;
            }
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                paths.push(expand_path(&current)?);
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    ensure!(
        quote.is_none() && !escaped,
        "unfinished quote or escape in file paths"
    );
    if !current.is_empty() {
        paths.push(expand_path(&current)?);
    }
    ensure!(!paths.is_empty(), "clipboard contains no file paths");
    Ok(paths)
}

fn expand_path(path: &str) -> Result<PathBuf> {
    if let Some(tail) = path.strip_prefix("~/") {
        Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?).join(tail))
    } else {
        Ok(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_drops_without_shell_execution() {
        assert_eq!(
            parse_paths("'/tmp/a b.pdf' /tmp/c\\ d.txt").unwrap(),
            vec![PathBuf::from("/tmp/a b.pdf"), PathBuf::from("/tmp/c d.txt")]
        );
        #[cfg(unix)]
        assert_eq!(
            parse_paths("file:///tmp/a%20b.pdf\r\n").unwrap(),
            vec![PathBuf::from("/tmp/a b.pdf")]
        );
        #[cfg(windows)]
        assert_eq!(
            parse_paths("file:///C:/tmp/a%20b.pdf\r\n").unwrap(),
            vec![PathBuf::from(r"C:\tmp\a b.pdf")]
        );
        assert!(parse_paths("file://other-host/tmp/a").is_err());
        assert!(parse_paths("'/tmp/broken").is_err());
    }
    #[cfg(windows)]
    #[test]
    fn windows_paths_keep_directory_separators() {
        let path = r"C:\Users\Ada\screenshot.PNG";
        assert_eq!(parse_paths(path).unwrap(), vec![PathBuf::from(path)]);
    }
    #[test]
    fn rejects_batch_before_transfer_and_snapshots_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.pdf");
        std::fs::write(&path, b"abc").unwrap();
        let mut limits = Limits {
            max_file_bytes: 2,
            ..Limits::default()
        };
        assert!(prepare(std::slice::from_ref(&path), &limits).is_err());
        limits.max_file_bytes = 3;
        let files = prepare(std::slice::from_ref(&path), &limits).unwrap();
        std::fs::write(&path, b"def").unwrap();
        assert_eq!(std::fs::read(files[0].snapshot.path()).unwrap(), b"abc");
        limits.deny_extensions = vec!["PDF".into()];
        assert!(prepare(std::slice::from_ref(&path), &limits).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, b"abc").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(path, &link).unwrap();
        assert!(prepare(&[link], &Limits::default()).is_err());
        assert!(prepare(&[PathBuf::from("/dev/null")], &Limits::default()).is_err());
    }
}
