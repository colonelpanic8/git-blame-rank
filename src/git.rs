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

use crate::core::{AuthorKey, FileAuthorStat, FileSummary};
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

pub fn resolve_repo_root(path: &Path) -> Result<PathBuf> {
    let repo = Repository::discover(path)
        .with_context(|| format!("failed to discover git repository from {}", path.display()))?;
    let workdir = repo.workdir().with_context(|| {
        format!(
            "repository at {} does not have a working directory",
            path.display()
        )
    })?;
    workdir
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root {}", workdir.display()))
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
    options.use_mailmap(true);
    let blame = repo
        .blame_file(&path_buf, Some(&mut options))
        .with_context(|| format!("failed to blame {}", String::from_utf8_lossy(path)))?;

    let mut authors: HashMap<AuthorKey, AuthorAccumulator> = HashMap::new();
    let mut total_lines = 0_u32;

    for hunk in blame.iter() {
        let lines = hunk.lines_in_hunk() as u32;
        total_lines += lines;

        let signature = hunk.final_signature();
        let author = AuthorKey {
            name: SmolStr::new(signature.name().unwrap_or("")),
            email: SmolStr::new(signature.email().unwrap_or("")),
        };

        let entry = authors.entry(author).or_default();
        entry.lines += lines;
        entry.commit_ids.push(hunk.final_commit_id());
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use git2::{Repository, Signature};
    use tempfile::TempDir;

    use super::*;

    fn init_repo() -> (TempDir, Repository) {
        let tempdir = TempDir::new().unwrap();
        let repo = Repository::init(tempdir.path()).unwrap();
        (tempdir, repo)
    }

    fn write_file(root: &Path, relative_path: &str, contents: &str) {
        let file_path = root.join(relative_path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(file_path, contents).unwrap();
    }

    fn commit_file(
        repo: &Repository,
        relative_path: &str,
        contents: &str,
        name: &str,
        email: &str,
        message: &str,
    ) -> Oid {
        write_file(repo.workdir().unwrap(), relative_path, contents);

        let mut index = repo.index().unwrap();
        index.add_path(Path::new(relative_path)).unwrap();
        index.write().unwrap();

        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now(name, email).unwrap();
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| repo.find_commit(oid).unwrap());
        let parents = parent.iter().collect::<Vec<_>>();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
    }

    #[test]
    fn dedupe_commit_ids_sorts_and_removes_duplicates() {
        let first = Oid::from_str(&"1".repeat(40)).unwrap();
        let second = Oid::from_str(&"2".repeat(40)).unwrap();

        let deduped = dedupe_commit_ids(vec![second, first, second, first]);

        assert_eq!(deduped, vec![first, second]);
    }

    #[test]
    fn discover_files_returns_sorted_blob_paths() {
        let (_tempdir, repo) = init_repo();
        commit_file(
            &repo,
            "z-last.txt",
            "z\n",
            "Alice",
            "alice@example.com",
            "add z",
        );
        commit_file(
            &repo,
            "nested/a-first.rs",
            "fn main() {}\n",
            "Bob",
            "bob@example.com",
            "add nested file",
        );

        let files = discover_files(repo.workdir().unwrap(), "HEAD").unwrap();

        assert_eq!(
            files,
            vec![
                BString::from("nested/a-first.rs"),
                BString::from("z-last.txt")
            ]
        );
    }

    #[test]
    fn resolve_repo_root_discovers_worktree_root_from_nested_directory() {
        let (_tempdir, repo) = init_repo();
        commit_file(
            &repo,
            "nested/deeper/file.txt",
            "content\n",
            "Alice",
            "alice@example.com",
            "initial",
        );

        let nested_dir = repo.workdir().unwrap().join("nested/deeper");
        let resolved = resolve_repo_root(&nested_dir).unwrap();

        assert_eq!(resolved, repo.workdir().unwrap().canonicalize().unwrap());
    }

    #[test]
    fn discover_files_returns_revision_errors() {
        let (_tempdir, repo) = init_repo();
        commit_file(
            &repo,
            "file.txt",
            "content\n",
            "Alice",
            "alice@example.com",
            "initial",
        );

        let error = discover_files(repo.workdir().unwrap(), "missing-ref")
            .unwrap_err()
            .to_string();

        assert!(error.contains("failed to resolve revision missing-ref"));
    }

    #[test]
    fn blame_file_aggregates_lines_per_author_and_dedupes_commit_ids() {
        let (_tempdir, repo) = init_repo();
        let alice_commit = commit_file(
            &repo,
            "src/lib.rs",
            "alpha\nbeta\ngamma\n",
            "Alice",
            "alice@example.com",
            "initial",
        );
        let bob_commit = commit_file(
            &repo,
            "src/lib.rs",
            "alpha\nBETA\ngamma\n",
            "Bob",
            "bob@example.com",
            "update middle line",
        );

        let summary = blame_file(
            &repo,
            resolve_commit_oid(&repo, "HEAD").unwrap(),
            &BString::from("src/lib.rs"),
        )
        .unwrap();

        assert_eq!(summary.path, BString::from("src/lib.rs"));
        assert_eq!(summary.total_lines, 3);
        assert_eq!(summary.authors.len(), 2);

        let alice = summary
            .authors
            .iter()
            .find(|stat| stat.author.email == "alice@example.com")
            .unwrap();
        assert_eq!(alice.lines, 2);
        assert_eq!(alice.commit_ids, vec![alice_commit]);

        let bob = summary
            .authors
            .iter()
            .find(|stat| stat.author.email == "bob@example.com")
            .unwrap();
        assert_eq!(bob.lines, 1);
        assert_eq!(bob.commit_ids, vec![bob_commit]);
    }

    #[test]
    fn blame_file_uses_mailmap_to_collapse_author_aliases() {
        let (_tempdir, repo) = init_repo();
        let canonical_commit = commit_file(
            &repo,
            "src/lib.rs",
            "alpha\nbeta\ngamma\n",
            "Alice Example",
            "alice@example.com",
            "initial",
        );
        let alias_commit = commit_file(
            &repo,
            "src/lib.rs",
            "alpha\nBETA\ngamma\n",
            "Alice Alias",
            "alice+alias@example.com",
            "update middle line",
        );
        commit_file(
            &repo,
            ".mailmap",
            "Alice Example <alice@example.com> Alice Alias <alice+alias@example.com>\n",
            "Maintainer",
            "maintainer@example.com",
            "add mailmap",
        );

        let summary = blame_file(
            &repo,
            resolve_commit_oid(&repo, "HEAD").unwrap(),
            &BString::from("src/lib.rs"),
        )
        .unwrap();

        assert_eq!(summary.authors.len(), 1);
        let author = &summary.authors[0];
        assert_eq!(author.author.name.as_str(), "Alice Example");
        assert_eq!(author.author.email.as_str(), "alice@example.com");
        assert_eq!(author.lines, 3);
        assert_eq!(
            author.commit_ids,
            dedupe_commit_ids(vec![canonical_commit, alias_commit])
        );
    }

    #[test]
    fn start_scan_emits_started_and_terminal_events_for_each_file() {
        let (_tempdir, repo) = init_repo();
        commit_file(
            &repo,
            "tracked.txt",
            "line one\nline two\n",
            "Alice",
            "alice@example.com",
            "initial",
        );

        let handle = start_scan(
            ScanConfig {
                repo_root: repo.workdir().unwrap().to_path_buf(),
                rev: Arc::<str>::from("HEAD"),
                jobs: 2,
            },
            &[BString::from("tracked.txt"), BString::from("missing.txt")],
        );
        let events = handle.event_rx.iter().collect::<Vec<_>>();
        handle.join();

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, WorkerEvent::Started { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, WorkerEvent::Finished { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, WorkerEvent::Failed { .. }))
                .count(),
            1
        );

        let finished = events
            .iter()
            .find_map(|event| match event {
                WorkerEvent::Finished { path, summary, .. } => Some((path, summary)),
                _ => None,
            })
            .unwrap();
        assert_eq!(finished.0, &BString::from("tracked.txt"));
        assert_eq!(finished.1.total_lines, 2);

        let failed = events
            .iter()
            .find_map(|event| match event {
                WorkerEvent::Failed { path, error, .. } => Some((path, error)),
                _ => None,
            })
            .unwrap();
        assert_eq!(failed.0, &BString::from("missing.txt"));
        assert!(failed.1.contains("failed to blame missing.txt"));
    }

    #[test]
    fn start_scan_reports_failures_when_repository_cannot_be_opened() {
        let missing_repo = TempDir::new().unwrap();
        let repo_root = missing_repo.path().join("does-not-exist");
        let handle = start_scan(
            ScanConfig {
                repo_root,
                rev: Arc::<str>::from("HEAD"),
                jobs: 1,
            },
            &[BString::from("a.rs"), BString::from("b.rs")],
        );
        let events = handle.event_rx.iter().collect::<Vec<_>>();
        handle.join();

        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| matches!(event, WorkerEvent::Failed { elapsed, .. } if *elapsed == Default::default())));
    }
}
