mod cli;
mod tui;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use cli::{Cli, OutputMode};
use git_blame_rank::core::ScanState;
use git_blame_rank::git::{ScanConfig, discover_files, resolve_repo_root, start_scan};
use git_blame_rank::settings::RepoSettings;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = resolve_repo_root(&cli.repo)?;
    let jobs = cli.jobs.unwrap_or_else(default_jobs).max(1);

    let settings_path = settings_path(&cli, &repo_root);
    let mut settings = match &settings_path {
        Some(path) => RepoSettings::load(path)?,
        None => RepoSettings::default(),
    };
    for path in &cli.ignore_paths {
        settings.ignore_path(path);
    }
    for extension in &cli.ignore_extensions {
        settings.ignore_extension(extension);
    }

    if cli.save_settings
        && let Some(path) = &settings_path
    {
        settings.save(path)?;
    }

    let files = discover_files(&repo_root, &cli.rev, &settings)?;
    let mut scan_state = ScanState::new(repo_root.clone(), cli.rev.clone(), jobs, files.clone());
    let scan_handle = start_scan(
        ScanConfig {
            repo_root,
            rev: cli.rev.clone().into(),
            jobs,
        },
        &files,
    );

    let run_result = match cli.mode {
        OutputMode::Tui => tui::run(
            &mut scan_state,
            &scan_handle.event_rx,
            settings,
            settings_path,
        ),
        OutputMode::Report => {
            while let Ok(worker_event) = scan_handle.event_rx.recv() {
                scan_state.apply_worker_event(worker_event);
                if scan_state.is_finished() {
                    break;
                }
            }
            print_report(&scan_state, &settings);
            Ok(())
        }
    };

    scan_handle.join();
    run_result
}

/// Resolves where per-repository settings live, or `None` when disabled.
fn settings_path(cli: &Cli, repo_root: &Path) -> Option<PathBuf> {
    if cli.no_settings {
        return None;
    }

    Some(
        cli.settings
            .clone()
            .unwrap_or_else(|| RepoSettings::settings_path(repo_root)),
    )
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().min(8))
        .unwrap_or(4)
}

fn print_report(scan_state: &ScanState, settings: &RepoSettings) {
    println!(
        "repo={} rev={} files={}/{} failures={} lines={}",
        scan_state.repo_root.display(),
        scan_state.rev,
        scan_state.processed_files,
        scan_state.total_files,
        scan_state.failed_files,
        scan_state.total_lines,
    );

    if !settings.is_empty() {
        println!(
            "ignored paths=[{}] extensions=[{}]",
            settings.ignored_paths.join(", "),
            settings.ignored_extensions.join(", "),
        );
    }

    for row in scan_state.all_author_rows() {
        println!(
            "{:>8} {:>6} {:>8}  {} <{}>",
            row.lines,
            row.files,
            row.commits,
            row.author.display_name(),
            row.author.email,
        );
    }
}
