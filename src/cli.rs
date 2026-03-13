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
