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
        ]);

        assert_eq!(cli.repo, PathBuf::from("/tmp/repo"));
        assert_eq!(cli.rev, "main~1");
        assert_eq!(cli.mode, OutputMode::Report);
        assert_eq!(cli.jobs, Some(8));
    }
}
