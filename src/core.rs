use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bstr::BString;
use git2::Oid;
use smol_str::SmolStr;

use crate::event::WorkerEvent;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorKey {
    pub name: SmolStr,
    pub email: SmolStr,
}

impl AuthorKey {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "<unknown>"
        } else {
            self.name.as_str()
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileAuthorStat {
    pub author: AuthorKey,
    pub lines: u32,
    pub commit_ids: Vec<Oid>,
}

#[derive(Clone, Debug)]
pub struct FileSummary {
    pub path: BString,
    pub total_lines: u32,
    pub authors: Vec<FileAuthorStat>,
}

#[derive(Debug)]
pub enum FileStatus {
    Pending,
    Running,
    Complete,
    Failed {
        #[allow(dead_code)]
        error: Arc<str>,
    },
}

#[derive(Debug)]
pub struct FileRecord {
    pub node_id: usize,
    pub extension: SmolStr,
    pub status: FileStatus,
    pub summary: Option<FileSummary>,
}

#[derive(Clone, Debug)]
pub struct RecentFile {
    pub path: String,
    pub lines: u32,
    pub status: RecentFileStatus,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug)]
pub enum RecentFileStatus {
    Complete,
    Failed,
}

#[derive(Debug, Default)]
struct AuthorAggregate {
    lines: u64,
    files: u64,
    commits: HashSet<Oid>,
}

#[derive(Clone, Debug)]
pub struct AuthorRow {
    pub author: AuthorKey,
    pub lines: u64,
    pub files: u64,
    pub commits: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NodeKind {
    Directory,
    File,
}

#[derive(Debug)]
pub struct TreeNode {
    pub name: SmolStr,
    pub path: BString,
    pub kind: NodeKind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub total_files: usize,
    pub processed_files: usize,
}

#[derive(Debug)]
pub struct ExtensionStat {
    pub extension: SmolStr,
    pub total_files: usize,
    pub processed_files: usize,
}

#[derive(Debug)]
pub struct ScanState {
    pub repo_root: PathBuf,
    pub rev: String,
    pub jobs: usize,
    pub started_at: Instant,
    pub completed_at: Option<Instant>,
    pub total_files: usize,
    pub processed_files: usize,
    pub failed_files: usize,
    pub total_lines: u64,
    pub file_records: HashMap<BString, FileRecord>,
    pub recent_files: VecDeque<RecentFile>,
    pub tree_nodes: Vec<TreeNode>,
    pub extensions: Vec<ExtensionStat>,
}

impl ScanState {
    pub fn new(repo_root: PathBuf, rev: String, jobs: usize, files: Vec<BString>) -> Self {
        let total_files = files.len();
        let mut tree_nodes = vec![TreeNode {
            name: SmolStr::new("."),
            path: BString::default(),
            kind: NodeKind::Directory,
            parent: None,
            children: Vec::new(),
            total_files: 0,
            processed_files: 0,
        }];
        let mut node_by_path = HashMap::new();
        node_by_path.insert(BString::default(), 0_usize);

        let mut file_records = HashMap::new();
        let mut extension_counts = HashMap::<SmolStr, usize>::new();

        for path in files {
            let mut current_node = 0_usize;
            tree_nodes[current_node].total_files += 1;
            let mut prefix = Vec::new();
            let components = path
                .as_slice()
                .split(|byte| *byte == b'/')
                .collect::<Vec<_>>();

            for (index, component) in components.iter().enumerate() {
                if !prefix.is_empty() {
                    prefix.push(b'/');
                }
                prefix.extend_from_slice(component);

                let node_path = BString::from(prefix.clone());
                let kind = if index + 1 == components.len() {
                    NodeKind::File
                } else {
                    NodeKind::Directory
                };

                let child_id = if let Some(child_id) = node_by_path.get(&node_path).copied() {
                    child_id
                } else {
                    let child_id = tree_nodes.len();
                    tree_nodes.push(TreeNode {
                        name: SmolStr::new(&String::from_utf8_lossy(component)),
                        path: node_path.clone(),
                        kind,
                        parent: Some(current_node),
                        children: Vec::new(),
                        total_files: 0,
                        processed_files: 0,
                    });
                    tree_nodes[current_node].children.push(child_id);
                    node_by_path.insert(node_path.clone(), child_id);
                    child_id
                };

                current_node = child_id;
                tree_nodes[current_node].total_files += 1;
            }

            let extension = extension_for_path(&path);
            *extension_counts.entry(extension.clone()).or_default() += 1;
            file_records.insert(
                path.clone(),
                FileRecord {
                    node_id: current_node,
                    extension,
                    status: FileStatus::Pending,
                    summary: None,
                },
            );
        }

        sort_tree_children(&mut tree_nodes, 0);

        let mut extensions = extension_counts
            .into_iter()
            .map(|(extension, total_files)| ExtensionStat {
                extension,
                total_files,
                processed_files: 0,
            })
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| left.extension.cmp(&right.extension));

        Self {
            repo_root,
            rev,
            jobs,
            started_at: Instant::now(),
            completed_at: None,
            total_files,
            processed_files: 0,
            failed_files: 0,
            total_lines: 0,
            file_records,
            recent_files: VecDeque::with_capacity(256),
            tree_nodes,
            extensions,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.processed_files >= self.total_files
    }

    pub fn running_files(&self) -> usize {
        self.file_records
            .values()
            .filter(|record| matches!(record.status, FileStatus::Running))
            .count()
    }

    pub fn elapsed(&self) -> Duration {
        match self.completed_at {
            Some(completed_at) => completed_at.duration_since(self.started_at),
            None => self.started_at.elapsed(),
        }
    }

    pub fn apply_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Started { path, worker_id } => {
                let _ = worker_id;
                if let Some(record) = self.file_records.get_mut(&path) {
                    record.status = FileStatus::Running;
                }
            }
            WorkerEvent::Finished {
                path,
                summary,
                elapsed,
            } => {
                self.processed_files += 1;
                self.total_lines += u64::from(summary.total_lines);

                let node_id = if let Some(record) = self.file_records.get_mut(&path) {
                    record.status = FileStatus::Complete;
                    record.summary = Some(summary.clone());
                    record.node_id
                } else {
                    0
                };

                self.bump_processed_counts(node_id, &path);
                self.push_recent_file(RecentFile {
                    path: display_path(&summary.path),
                    lines: summary.total_lines,
                    status: RecentFileStatus::Complete,
                    elapsed,
                });
            }
            WorkerEvent::Failed {
                path,
                error,
                elapsed,
            } => {
                self.processed_files += 1;
                self.failed_files += 1;

                let node_id = if let Some(record) = self.file_records.get_mut(&path) {
                    record.status = FileStatus::Failed {
                        error: Arc::clone(&error),
                    };
                    record.node_id
                } else {
                    0
                };

                self.bump_processed_counts(node_id, &path);
                self.push_recent_file(RecentFile {
                    path: display_path(&path),
                    lines: 0,
                    status: RecentFileStatus::Failed,
                    elapsed,
                });
            }
        }

        if self.completed_at.is_none() && self.is_finished() {
            self.completed_at = Some(Instant::now());
        }
    }

    pub fn all_author_rows(&self) -> Vec<AuthorRow> {
        self.author_rows_filtered(|_| true)
    }

    pub fn author_rows_filtered<F>(&self, mut include: F) -> Vec<AuthorRow>
    where
        F: FnMut(&FileRecord) -> bool,
    {
        let mut aggregates = HashMap::<AuthorKey, AuthorAggregate>::new();

        for record in self.file_records.values() {
            let Some(summary) = &record.summary else {
                continue;
            };

            if !include(record) {
                continue;
            }

            for author_stat in &summary.authors {
                let entry = aggregates.entry(author_stat.author.clone()).or_default();
                entry.lines += u64::from(author_stat.lines);
                entry.files += 1;
                entry.commits.extend(author_stat.commit_ids.iter().copied());
            }
        }

        let mut rows = aggregates
            .into_iter()
            .map(|(author, aggregate)| AuthorRow {
                author,
                lines: aggregate.lines,
                files: aggregate.files,
                commits: aggregate.commits.len() as u64,
            })
            .collect::<Vec<_>>();

        rows.sort_by(|left, right| {
            right
                .lines
                .cmp(&left.lines)
                .then_with(|| right.files.cmp(&left.files))
                .then_with(|| left.author.cmp(&right.author))
        });
        rows
    }

    pub fn file_counts_filtered<F>(&self, mut include: F) -> (usize, usize)
    where
        F: FnMut(&FileRecord) -> bool,
    {
        let mut matching_files = 0;
        let mut processed_files = 0;

        for record in self.file_records.values() {
            if !include(record) {
                continue;
            }

            matching_files += 1;
            if record.summary.is_some() || matches!(record.status, FileStatus::Failed { .. }) {
                processed_files += 1;
            }
        }

        (processed_files, matching_files)
    }

    fn bump_processed_counts(&mut self, node_id: usize, path: &BString) {
        let mut cursor = Some(node_id);
        while let Some(current) = cursor {
            self.tree_nodes[current].processed_files += 1;
            cursor = self.tree_nodes[current].parent;
        }

        let extension = extension_for_path(path);
        if let Some(stat) = self
            .extensions
            .iter_mut()
            .find(|stat| stat.extension == extension)
        {
            stat.processed_files += 1;
        }
    }

    fn push_recent_file(&mut self, recent_file: RecentFile) {
        if self.recent_files.len() == 256 {
            self.recent_files.pop_back();
        }
        self.recent_files.push_front(recent_file);
    }
}

fn sort_tree_children(tree_nodes: &mut [TreeNode], node_id: usize) {
    let mut sorted_children = tree_nodes[node_id].children.clone();
    sorted_children.sort_by(|left, right| {
        let left_node = &tree_nodes[*left];
        let right_node = &tree_nodes[*right];
        left_node
            .kind
            .cmp(&right_node.kind)
            .then_with(|| left_node.name.cmp(&right_node.name))
    });
    tree_nodes[node_id].children = sorted_children;

    let children = tree_nodes[node_id].children.clone();
    for child_id in children {
        if tree_nodes[child_id].kind == NodeKind::Directory {
            sort_tree_children(tree_nodes, child_id);
        }
    }
}

fn extension_for_path(path: &BString) -> SmolStr {
    let display = display_path(path);
    let filename = display.rsplit('/').next().unwrap_or(display.as_str());
    match filename.rsplit_once('.') {
        Some((_, extension)) if !extension.is_empty() => SmolStr::new(extension),
        _ => SmolStr::new("[no ext]"),
    }
}

pub fn display_path(path: &BString) -> String {
    String::from_utf8_lossy(path.as_slice()).into_owned()
}

#[cfg(test)]
mod tests {
    use git2::Oid;

    use super::*;

    fn author(name: &str, email: &str) -> AuthorKey {
        AuthorKey {
            name: SmolStr::new(name),
            email: SmolStr::new(email),
        }
    }

    fn oid(hex_digit: char) -> Oid {
        Oid::from_str(&hex_digit.to_string().repeat(40)).unwrap()
    }

    fn author_stat(name: &str, email: &str, lines: u32, commit_ids: Vec<Oid>) -> FileAuthorStat {
        FileAuthorStat {
            author: author(name, email),
            lines,
            commit_ids,
        }
    }

    fn summary(path: &str, total_lines: u32, authors: Vec<FileAuthorStat>) -> FileSummary {
        FileSummary {
            path: BString::from(path),
            total_lines,
            authors,
        }
    }

    fn find_node_id(scan_state: &ScanState, path: &str) -> usize {
        scan_state
            .tree_nodes
            .iter()
            .position(|node| node.path == BString::from(path))
            .unwrap()
    }

    #[test]
    fn author_display_name_uses_unknown_for_empty_name() {
        assert_eq!(
            author("", "unknown@example.com").display_name(),
            "<unknown>"
        );
        assert_eq!(author("Alice", "alice@example.com").display_name(), "Alice");
    }

    #[test]
    fn scan_state_new_builds_tree_and_extension_indexes() {
        let scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            4,
            vec![
                BString::from("src/main.rs"),
                BString::from("src/lib.rs"),
                BString::from("README"),
            ],
        );

        assert_eq!(scan_state.total_files, 3);
        assert_eq!(scan_state.tree_nodes[0].total_files, 3);
        assert_eq!(
            scan_state.tree_nodes[0]
                .children
                .iter()
                .map(|child_id| scan_state.tree_nodes[*child_id].name.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "README"]
        );
        assert_eq!(find_node_id(&scan_state, "src"), 1);
        assert_eq!(scan_state.file_records.len(), 3);
        assert!(
            scan_state
                .file_records
                .values()
                .all(|record| matches!(record.status, FileStatus::Pending))
        );
        assert_eq!(
            scan_state
                .extensions
                .iter()
                .map(|stat| (
                    stat.extension.as_str(),
                    stat.total_files,
                    stat.processed_files
                ))
                .collect::<Vec<_>>(),
            vec![("[no ext]", 1, 0), ("rs", 2, 0)]
        );
    }

    #[test]
    fn apply_worker_event_marks_finished_files_and_updates_aggregates() {
        let mut scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            2,
            vec![BString::from("src/main.rs")],
        );
        let file_path = BString::from("src/main.rs");
        let file_node_id = scan_state.file_records[&file_path].node_id;
        let parent_node_id = scan_state.tree_nodes[file_node_id].parent.unwrap();

        scan_state.apply_worker_event(WorkerEvent::Started {
            path: file_path.clone(),
            worker_id: 0,
        });
        assert_eq!(scan_state.running_files(), 1);

        let file_summary = summary(
            "src/main.rs",
            5,
            vec![
                author_stat("Alice", "alice@example.com", 3, vec![oid('1')]),
                author_stat("Bob", "bob@example.com", 2, vec![oid('2')]),
            ],
        );
        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: file_path.clone(),
            summary: file_summary.clone(),
            elapsed: Duration::from_millis(25),
        });

        assert_eq!(scan_state.processed_files, 1);
        assert_eq!(scan_state.total_lines, 5);
        assert_eq!(scan_state.failed_files, 0);
        assert!(scan_state.completed_at.is_some());
        assert!(matches!(
            scan_state.file_records[&file_path].status,
            FileStatus::Complete
        ));
        assert_eq!(
            scan_state.file_records[&file_path]
                .summary
                .as_ref()
                .unwrap()
                .total_lines,
            5
        );
        assert_eq!(scan_state.tree_nodes[0].processed_files, 1);
        assert_eq!(scan_state.tree_nodes[parent_node_id].processed_files, 1);
        assert_eq!(scan_state.tree_nodes[file_node_id].processed_files, 1);
        assert_eq!(scan_state.extensions[0].processed_files, 1);
        assert_eq!(scan_state.recent_files.len(), 1);
        assert_eq!(scan_state.recent_files[0].path, "src/main.rs");
        assert_eq!(scan_state.recent_files[0].lines, 5);
        assert!(matches!(
            scan_state.recent_files[0].status,
            RecentFileStatus::Complete
        ));
        assert_eq!(scan_state.running_files(), 0);
        assert!(scan_state.is_finished());
    }

    #[test]
    fn apply_worker_event_marks_failed_files_and_completes_scan() {
        let mut scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            1,
            vec![BString::from("README.md")],
        );
        let file_path = BString::from("README.md");

        scan_state.apply_worker_event(WorkerEvent::Failed {
            path: file_path.clone(),
            error: Arc::<str>::from("boom"),
            elapsed: Duration::from_millis(10),
        });

        assert_eq!(scan_state.processed_files, 1);
        assert_eq!(scan_state.failed_files, 1);
        assert_eq!(scan_state.total_lines, 0);
        assert!(scan_state.completed_at.is_some());
        assert!(matches!(
            scan_state.file_records[&file_path].status,
            FileStatus::Failed { .. }
        ));
        assert_eq!(scan_state.tree_nodes[0].processed_files, 1);
        assert_eq!(scan_state.extensions[0].processed_files, 1);
        assert!(matches!(
            scan_state.recent_files[0].status,
            RecentFileStatus::Failed
        ));
    }

    #[test]
    fn author_rows_filtered_aggregates_lines_files_and_unique_commits() {
        let mut scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            2,
            vec![BString::from("src/lib.rs"), BString::from("notes.txt")],
        );

        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: BString::from("src/lib.rs"),
            summary: summary(
                "src/lib.rs",
                7,
                vec![
                    author_stat("Alice", "alice@example.com", 4, vec![oid('1'), oid('1')]),
                    author_stat("Bob", "bob@example.com", 3, vec![oid('2')]),
                ],
            ),
            elapsed: Duration::default(),
        });
        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: BString::from("notes.txt"),
            summary: summary(
                "notes.txt",
                10,
                vec![
                    author_stat("Alice", "alice@example.com", 6, vec![oid('1'), oid('3')]),
                    author_stat("Cara", "cara@example.com", 4, vec![oid('4')]),
                ],
            ),
            elapsed: Duration::default(),
        });

        let rows = scan_state.all_author_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].author.name.as_str(), "Alice");
        assert_eq!(rows[0].lines, 10);
        assert_eq!(rows[0].files, 2);
        assert_eq!(rows[0].commits, 2);
        assert_eq!(rows[1].author.name.as_str(), "Cara");
        assert_eq!(rows[2].author.name.as_str(), "Bob");

        let only_rs = scan_state.author_rows_filtered(|record| record.extension == "rs");
        assert_eq!(only_rs.len(), 2);
        assert_eq!(only_rs[0].author.name.as_str(), "Alice");
        assert_eq!(only_rs[0].lines, 4);
        assert_eq!(only_rs[0].files, 1);
        assert_eq!(only_rs[0].commits, 1);
    }

    #[test]
    fn file_counts_filtered_only_count_finished_and_failed_files_as_processed() {
        let mut scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            2,
            vec![
                BString::from("src/lib.rs"),
                BString::from("src/main.rs"),
                BString::from("README.md"),
            ],
        );

        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: BString::from("src/lib.rs"),
            summary: summary(
                "src/lib.rs",
                3,
                vec![author_stat("Alice", "a@example.com", 3, vec![oid('1')])],
            ),
            elapsed: Duration::default(),
        });
        scan_state.apply_worker_event(WorkerEvent::Failed {
            path: BString::from("README.md"),
            error: Arc::<str>::from("nope"),
            elapsed: Duration::default(),
        });

        let counts = scan_state.file_counts_filtered(|record| record.extension == "rs");
        assert_eq!(counts, (1, 2));

        let all_counts = scan_state.file_counts_filtered(|_| true);
        assert_eq!(all_counts, (2, 3));
    }

    #[test]
    fn recent_files_are_capped_to_256_entries() {
        let files = (0..257)
            .map(|index| BString::from(format!("file-{index}.rs")))
            .collect::<Vec<_>>();
        let mut scan_state = ScanState::new(PathBuf::from("/repo"), "HEAD".to_owned(), 1, files);

        for index in 0..257 {
            scan_state.apply_worker_event(WorkerEvent::Failed {
                path: BString::from(format!("file-{index}.rs")),
                error: Arc::<str>::from("failed"),
                elapsed: Duration::default(),
            });
        }

        assert_eq!(scan_state.recent_files.len(), 256);
        assert_eq!(scan_state.recent_files.front().unwrap().path, "file-256.rs");
        assert_eq!(scan_state.recent_files.back().unwrap().path, "file-1.rs");
    }

    #[test]
    fn extension_for_path_handles_missing_and_present_extensions() {
        assert_eq!(extension_for_path(&BString::from("src/main.rs")), "rs");
        assert_eq!(extension_for_path(&BString::from("README")), "[no ext]");
        assert_eq!(
            extension_for_path(&BString::from(".gitignore")),
            "gitignore"
        );
    }

    #[test]
    fn display_path_uses_lossy_utf8_conversion() {
        let path = BString::from(vec![b'a', b'/', 0xff, b'b']);
        assert_eq!(display_path(&path), "a/\u{fffd}b");
    }
}
