use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputMode {
    Tui,
    Report,
}

#[derive(Debug, Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    #[arg(long, default_value = "HEAD")]
    pub rev: String,

    #[arg(long, value_enum, default_value_t = OutputMode::Tui)]
    pub mode: OutputMode,

    #[arg(long)]
    pub jobs: Option<usize>,

    /// Path to the per-repository settings file.
    ///
    /// Defaults to `.git-blame-rank.toml` at the repository root.
    #[arg(long, value_name = "FILE")]
    pub settings: Option<PathBuf>,

    /// Scan everything, ignoring any persisted settings file.
    #[arg(long, conflicts_with = "settings")]
    pub no_settings: bool,

    /// Additionally ignore a repository-relative file or directory.
    #[arg(long = "ignore", value_name = "PATH")]
    pub ignore_paths: Vec<String>,

    /// Additionally ignore a file extension, written without a leading dot.
    #[arg(long = "ignore-ext", value_name = "EXT")]
    pub ignore_extensions: Vec<String>,

    /// Write the effective ignore rules back to the settings file before scanning.
    #[arg(long, conflicts_with = "no_settings")]
    pub save_settings: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_arguments() {
        let cli = Cli::parse_from(["git-blame-rank"]);

        assert_eq!(cli.repo, PathBuf::from("."));
        assert_eq!(cli.rev, "HEAD");
        assert_eq!(cli.mode, OutputMode::Tui);
        assert_eq!(cli.jobs, None);
        assert_eq!(cli.settings, None);
        assert!(!cli.no_settings);
        assert!(cli.ignore_paths.is_empty());
        assert!(cli.ignore_extensions.is_empty());
        assert!(!cli.save_settings);
    }

    #[test]
    fn parses_explicit_arguments() {
        let cli = Cli::parse_from([
            "git-blame-rank",
            "--repo",
            "/tmp/repo",
            "--rev",
            "main~1",
            "--mode",
            "report",
            "--jobs",
            "8",
            "--settings",
            "/tmp/settings.toml",
            "--ignore",
            "vendor",
            "--ignore",
            "docs",
            "--ignore-ext",
            "lock",
            "--save-settings",
        ]);

        assert_eq!(cli.repo, PathBuf::from("/tmp/repo"));
        assert_eq!(cli.rev, "main~1");
        assert_eq!(cli.mode, OutputMode::Report);
        assert_eq!(cli.jobs, Some(8));
        assert_eq!(cli.settings, Some(PathBuf::from("/tmp/settings.toml")));
        assert_eq!(cli.ignore_paths, vec!["vendor", "docs"]);
        assert_eq!(cli.ignore_extensions, vec!["lock"]);
        assert!(cli.save_settings);
    }

    #[test]
    fn rejects_settings_overrides_combined_with_no_settings() {
        assert!(
            Cli::try_parse_from(["git-blame-rank", "--no-settings", "--settings", "s.toml"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["git-blame-rank", "--no-settings", "--save-settings"]).is_err()
        );
    }
}
