use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::extension_for_path;

/// Name of the per-repository settings file, stored at the repository root.
pub const SETTINGS_FILE_NAME: &str = ".git-blame-rank.toml";

const SETTINGS_HEADER: &str = "\
# git-blame-rank per-repository settings.
# Paths are repository-relative and match a file or an entire directory subtree.
# Ignored entries are skipped during discovery, so they are never blamed.
";

/// Persistent per-repository configuration describing what a scan counts.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoSettings {
    /// Repository-relative files or directories excluded from the scan.
    pub ignored_paths: Vec<String>,
    /// File extensions excluded from the scan, without a leading dot.
    pub ignored_extensions: Vec<String>,
}

impl RepoSettings {
    pub fn settings_path(repo_root: &Path) -> PathBuf {
        repo_root.join(SETTINGS_FILE_NAME)
    }

    /// Loads settings from `path`, returning defaults when the file is absent.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings from {}", path.display()));
            }
        };

        let mut settings: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse settings file {}", path.display()))?;
        settings.normalize();
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut normalized = self.clone();
        normalized.normalize();

        let body = toml::to_string_pretty(&normalized).context("failed to encode settings")?;
        fs::write(path, format!("{SETTINGS_HEADER}\n{body}"))
            .with_context(|| format!("failed to write settings to {}", path.display()))
    }

    pub fn is_empty(&self) -> bool {
        self.ignored_paths.is_empty() && self.ignored_extensions.is_empty()
    }

    /// Returns true when a repository-relative path is excluded by any rule.
    pub fn is_ignored(&self, path: &[u8]) -> bool {
        self.is_path_ignored(path) || self.is_extension_ignored(&extension_for_path(path))
    }

    /// Returns true when `path` names an ignored entry or lives beneath one.
    pub fn is_path_ignored(&self, path: &[u8]) -> bool {
        self.ignored_paths
            .iter()
            .any(|ignored| path_covers(ignored.as_bytes(), path))
    }

    pub fn is_extension_ignored(&self, extension: &str) -> bool {
        self.ignored_extensions
            .iter()
            .any(|ignored| ignored == extension)
    }

    /// Adds an ignore rule, dropping entries that the new rule already covers.
    pub fn ignore_path(&mut self, path: &str) {
        let Some(normalized) = normalize_path(path) else {
            return;
        };

        if self.is_path_ignored(normalized.as_bytes()) {
            return;
        }

        self.ignored_paths
            .retain(|existing| !path_covers(normalized.as_bytes(), existing.as_bytes()));
        self.ignored_paths.push(normalized);
        self.ignored_paths.sort();
    }

    /// Removes an ignore rule along with any rules beneath it.
    pub fn unignore_path(&mut self, path: &str) {
        let Some(normalized) = normalize_path(path) else {
            return;
        };

        self.ignored_paths
            .retain(|existing| !path_covers(normalized.as_bytes(), existing.as_bytes()));
    }

    pub fn ignore_extension(&mut self, extension: &str) {
        let extension = extension.trim();
        if extension.is_empty() || self.is_extension_ignored(extension) {
            return;
        }

        self.ignored_extensions.push(extension.to_owned());
        self.ignored_extensions.sort();
    }

    fn normalize(&mut self) {
        let mut paths = std::mem::take(&mut self.ignored_paths);
        paths.sort();
        paths.dedup();
        for path in paths {
            self.ignore_path(&path);
        }

        let mut extensions = std::mem::take(&mut self.ignored_extensions);
        extensions.sort();
        extensions.dedup();
        for extension in extensions {
            self.ignore_extension(&extension);
        }
    }
}

/// Trims a user-supplied path into the repository-relative form Git reports.
fn normalize_path(path: &str) -> Option<String> {
    let mut trimmed = path.trim();
    while let Some(rest) = trimmed.strip_prefix("./") {
        trimmed = rest;
    }
    let trimmed = trimmed.trim_matches('/');

    if trimmed.is_empty() || trimmed == "." {
        return None;
    }

    Some(trimmed.to_owned())
}

/// Returns true when `ancestor` is `path` itself or a directory containing it.
fn path_covers(ancestor: &[u8], path: &[u8]) -> bool {
    path.starts_with(ancestor) && (path.len() == ancestor.len() || path[ancestor.len()] == b'/')
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn load_returns_defaults_when_file_is_missing() {
        let tempdir = TempDir::new().unwrap();
        let settings = RepoSettings::load(&RepoSettings::settings_path(tempdir.path())).unwrap();

        assert_eq!(settings, RepoSettings::default());
        assert!(settings.is_empty());
    }

    #[test]
    fn save_and_load_round_trips_normalized_rules() {
        let tempdir = TempDir::new().unwrap();
        let path = RepoSettings::settings_path(tempdir.path());
        let mut settings = RepoSettings::default();
        settings.ignore_path("./vendor/");
        settings.ignore_path("docs");
        settings.ignore_extension("lock");
        settings.save(&path).unwrap();

        let loaded = RepoSettings::load(&path).unwrap();

        assert_eq!(loaded.ignored_paths, vec!["docs", "vendor"]);
        assert_eq!(loaded.ignored_extensions, vec!["lock"]);
    }

    #[test]
    fn load_reports_parse_errors_with_the_file_path() {
        let tempdir = TempDir::new().unwrap();
        let path = RepoSettings::settings_path(tempdir.path());
        fs::write(&path, "ignored_paths = 3\n").unwrap();

        let error = RepoSettings::load(&path).unwrap_err().to_string();

        assert!(error.contains("failed to parse settings file"));
    }

    #[test]
    fn ignore_path_collapses_redundant_descendants() {
        let mut settings = RepoSettings::default();
        settings.ignore_path("vendor/gems");
        settings.ignore_path("vendor");
        settings.ignore_path("vendor/other");

        assert_eq!(settings.ignored_paths, vec!["vendor"]);
    }

    #[test]
    fn ignore_rules_match_subtrees_but_not_sibling_prefixes() {
        let mut settings = RepoSettings::default();
        settings.ignore_path("src/vendor");

        assert!(settings.is_path_ignored(b"src/vendor"));
        assert!(settings.is_path_ignored(b"src/vendor/nested/file.rs"));
        assert!(!settings.is_path_ignored(b"src/vendored/file.rs"));
        assert!(!settings.is_path_ignored(b"src/lib.rs"));
    }

    #[test]
    fn unignore_path_removes_the_rule_and_its_descendants() {
        let mut settings = RepoSettings::default();
        settings.ignore_path("docs");
        settings.ignore_path("vendor/gems");
        settings.ignore_path("vendor/other");

        settings.unignore_path("vendor");

        assert_eq!(settings.ignored_paths, vec!["docs"]);
    }

    #[test]
    fn is_ignored_covers_extension_rules() {
        let mut settings = RepoSettings::default();
        settings.ignore_extension("lock");

        assert!(settings.is_ignored(b"Cargo.lock"));
        assert!(!settings.is_ignored(b"Cargo.toml"));
    }

    #[test]
    fn normalize_path_rejects_empty_and_root_forms() {
        assert_eq!(normalize_path("  vendor/  "), Some("vendor".to_owned()));
        assert_eq!(normalize_path("./"), None);
        assert_eq!(normalize_path("/"), None);
        assert_eq!(normalize_path("."), None);
    }
}
