mod cli;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, OutputMode};
use git_blame_rank::core::ScanState;
use git_blame_rank::git::{ScanConfig, discover_files, resolve_repo_root, start_scan};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = resolve_repo_root(&cli.repo)?;
    let jobs = cli.jobs.unwrap_or_else(default_jobs).max(1);

    let files = discover_files(&repo_root, &cli.rev)?;
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
        OutputMode::Tui => tui::run(&mut scan_state, &scan_handle.event_rx),
        OutputMode::Report => {
            while let Ok(worker_event) = scan_handle.event_rx.recv() {
                scan_state.apply_worker_event(worker_event);
                if scan_state.is_finished() {
                    break;
                }
            }
            print_report(&scan_state);
            Ok(())
        }
    };

    scan_handle.join();
    run_result
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().min(8))
        .unwrap_or(4)
}

fn print_report(scan_state: &ScanState) {
    println!(
        "repo={} rev={} files={}/{} failures={} lines={}",
        scan_state.repo_root.display(),
        scan_state.rev,
        scan_state.processed_files,
        scan_state.total_files,
        scan_state.failed_files,
        scan_state.total_lines,
    );

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
