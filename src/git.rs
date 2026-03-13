use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use bstr::BString;
use crossbeam_channel::{Receiver, Sender, unbounded};
use git2::{BlameOptions, ObjectType, Oid, Repository, Tree};
use smol_str::SmolStr;

use crate::app::{AuthorKey, FileAuthorStat, FileSummary};
use crate::event::WorkerEvent;

#[derive(Clone, Debug)]
pub struct ScanConfig {
    pub repo_root: PathBuf,
    pub rev: Arc<str>,
    pub jobs: usize,
}

pub struct ScanHandle {
    pub event_rx: Receiver<WorkerEvent>,
    join_handles: Vec<thread::JoinHandle<()>>,
}

impl ScanHandle {
    pub fn join(self) {
        for handle in self.join_handles {
            let _ = handle.join();
        }
    }
}

pub fn discover_files(repo_root: &Path, rev: &str) -> Result<Vec<BString>> {
    let repo = Repository::open(repo_root)
        .with_context(|| format!("failed to open repository at {}", repo_root.display()))?;
    let tree = resolve_tree(&repo, rev)?;
    let mut files = Vec::new();
    walk_tree(&repo, &tree, Vec::new(), &mut files)?;
    files.sort();
    Ok(files)
}

pub fn start_scan(config: ScanConfig, files: &[BString]) -> ScanHandle {
    let (event_tx, event_rx) = unbounded();
    let (work_tx, work_rx) = unbounded();

    for path in files {
        if work_tx.send(path.clone()).is_err() {
            break;
        }
    }
    drop(work_tx);

    let join_handles = (0..config.jobs)
        .map(|worker_id| {
            let event_tx = event_tx.clone();
            let work_rx = work_rx.clone();
            let config = config.clone();
            thread::spawn(move || worker_loop(worker_id, config, work_rx, event_tx))
        })
        .collect();

    ScanHandle {
        event_rx,
        join_handles,
    }
}

fn worker_loop(
    worker_id: usize,
    config: ScanConfig,
    work_rx: Receiver<BString>,
    event_tx: Sender<WorkerEvent>,
) {
    let repo = match Repository::open(&config.repo_root) {
        Ok(repo) => repo,
        Err(error) => {
            let message = Arc::<str>::from(error.to_string());
            while let Ok(path) = work_rx.recv() {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path,
                    error: Arc::clone(&message),
                    elapsed: Default::default(),
                });
            }
            return;
        }
    };

    let rev_oid = match resolve_commit_oid(&repo, &config.rev) {
        Ok(oid) => oid,
        Err(error) => {
            let message = Arc::<str>::from(error.to_string());
            while let Ok(path) = work_rx.recv() {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path,
                    error: Arc::clone(&message),
                    elapsed: Default::default(),
                });
            }
            return;
        }
    };

    while let Ok(path) = work_rx.recv() {
        let _ = event_tx.send(WorkerEvent::Started {
            path: path.clone(),
            worker_id,
        });

        let started_at = Instant::now();
        match blame_file(&repo, rev_oid, &path) {
            Ok(summary) => {
                let _ = event_tx.send(WorkerEvent::Finished {
                    path,
                    summary,
                    elapsed: started_at.elapsed(),
                });
            }
            Err(error) => {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path,
                    error: Arc::<str>::from(error.to_string()),
                    elapsed: started_at.elapsed(),
                });
            }
        }
    }
}

fn resolve_tree<'repo>(repo: &'repo Repository, rev: &str) -> Result<Tree<'repo>> {
    let object = repo
        .revparse_single(rev)
        .with_context(|| format!("failed to resolve revision {rev}"))?;
    let commit = object
        .peel_to_commit()
        .with_context(|| format!("revision {rev} does not point to a commit"))?;
    commit.tree().context("failed to load tree for revision")
}

fn resolve_commit_oid(repo: &Repository, rev: &str) -> Result<Oid> {
    let object = repo
        .revparse_single(rev)
        .with_context(|| format!("failed to resolve revision {rev}"))?;
    let commit = object
        .peel_to_commit()
        .with_context(|| format!("revision {rev} does not point to a commit"))?;
    Ok(commit.id())
}

fn walk_tree(
    repo: &Repository,
    tree: &Tree<'_>,
    prefix: Vec<u8>,
    files: &mut Vec<BString>,
) -> Result<()> {
    for entry in tree.iter() {
        let mut path = prefix.clone();
        path.extend_from_slice(entry.name_bytes());

        match entry.kind() {
            Some(ObjectType::Blob) => files.push(BString::from(path)),
            Some(ObjectType::Tree) => {
                path.push(b'/');
                let subtree = repo
                    .find_tree(entry.id())
                    .with_context(|| "failed to load subtree while walking repository")?;
                walk_tree(repo, &subtree, path, files)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn blame_file(repo: &Repository, rev_oid: Oid, path: &BString) -> Result<FileSummary> {
    let path_buf = pathbuf_from_bstring(path);
    let mut options = BlameOptions::new();
    options.newest_commit(rev_oid);
    let blame = repo
        .blame_file(&path_buf, Some(&mut options))
        .with_context(|| format!("failed to blame {}", String::from_utf8_lossy(path)))?;

    let mut authors: HashMap<AuthorKey, AuthorAccumulator> = HashMap::new();
    let mut total_lines = 0_u32;

    for hunk in blame.iter() {
        let lines = hunk.lines_in_hunk() as u32;
        total_lines += lines;

        let commit = repo
            .find_commit(hunk.final_commit_id())
            .with_context(|| "failed to look up blamed commit")?;
        let signature = commit.author();
        let author = AuthorKey {
            name: SmolStr::new(signature.name().unwrap_or("")),
            email: SmolStr::new(signature.email().unwrap_or("")),
        };

        let entry = authors.entry(author).or_default();
        entry.lines += lines;
        entry.commit_ids.push(commit.id());
    }

    let authors = authors
        .into_iter()
        .map(|(author, accumulator)| FileAuthorStat {
            author,
            lines: accumulator.lines,
            commit_ids: dedupe_commit_ids(accumulator.commit_ids),
        })
        .collect::<Vec<_>>();

    Ok(FileSummary {
        path: path.clone(),
        total_lines,
        authors,
    })
}

#[derive(Default)]
struct AuthorAccumulator {
    lines: u32,
    commit_ids: Vec<Oid>,
}

fn dedupe_commit_ids(mut commit_ids: Vec<Oid>) -> Vec<Oid> {
    commit_ids.sort_unstable();
    commit_ids.dedup();
    commit_ids
}

#[cfg(unix)]
fn pathbuf_from_bstring(path: &BString) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn pathbuf_from_bstring(path: &BString) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path.as_slice()).into_owned())
}
