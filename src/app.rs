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
pub struct AuthorAggregate {
    pub lines: u64,
    pub files: u64,
    pub commits: HashSet<Oid>,
}

#[derive(Clone, Debug)]
pub struct AuthorRow {
    pub author: AuthorKey,
    pub lines: u64,
    pub files: u64,
    pub commits: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusPane {
    Tree,
    Extensions,
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
    pub expanded: bool,
    pub enabled: bool,
    pub total_files: usize,
    pub processed_files: usize,
}

#[derive(Clone, Debug)]
pub struct VisibleTreeNode {
    pub node_id: usize,
    pub depth: usize,
}

#[derive(Debug)]
pub struct ExtensionFilter {
    pub extension: SmolStr,
    pub enabled: bool,
    pub total_files: usize,
    pub processed_files: usize,
}

#[derive(Debug)]
pub struct AppState {
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
    pub selected_tree_node: usize,
    pub extension_filters: Vec<ExtensionFilter>,
    pub selected_extension: usize,
    pub focus: FocusPane,
}

impl AppState {
    pub fn new(repo_root: PathBuf, rev: String, jobs: usize, files: Vec<BString>) -> Self {
        let total_files = files.len();
        let mut tree_nodes = vec![TreeNode {
            name: SmolStr::new("."),
            path: BString::default(),
            kind: NodeKind::Directory,
            parent: None,
            children: Vec::new(),
            expanded: true,
            enabled: true,
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
                        expanded: false,
                        enabled: true,
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

        let mut extension_filters = extension_counts
            .into_iter()
            .map(|(extension, total_files)| ExtensionFilter {
                extension,
                enabled: true,
                total_files,
                processed_files: 0,
            })
            .collect::<Vec<_>>();
        extension_filters.sort_by(|left, right| left.extension.cmp(&right.extension));

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
            recent_files: VecDeque::with_capacity(16),
            tree_nodes,
            selected_tree_node: 0,
            extension_filters,
            selected_extension: 0,
            focus: FocusPane::Tree,
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

    pub fn author_rows(&self) -> Vec<AuthorRow> {
        let selected_scope = self.selected_tree_node;
        let mut aggregates = HashMap::<AuthorKey, AuthorAggregate>::new();

        for record in self.file_records.values() {
            let Some(summary) = &record.summary else {
                continue;
            };

            if !self.file_in_scope(record, selected_scope) {
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

    pub fn current_scope_label(&self) -> String {
        if self.selected_tree_node == 0 {
            ".".to_owned()
        } else {
            display_path(&self.tree_nodes[self.selected_tree_node].path)
        }
    }

    pub fn current_scope_file_counts(&self) -> (usize, usize) {
        let selected_scope = self.selected_tree_node;
        let mut matching_files = 0;
        let mut processed_files = 0;

        for record in self.file_records.values() {
            if !self.file_matches_view_filters(record) {
                continue;
            }
            if self.is_descendant(record.node_id, selected_scope) {
                matching_files += 1;
                if record.summary.is_some() || matches!(record.status, FileStatus::Failed { .. }) {
                    processed_files += 1;
                }
            }
        }

        (processed_files, matching_files)
    }

    pub fn visible_tree_nodes(&self) -> Vec<VisibleTreeNode> {
        let mut visible = Vec::new();
        self.push_visible_tree_nodes(0, 0, &mut visible);
        visible
    }

    pub fn move_tree_selection(&mut self, delta: isize) {
        let visible = self.visible_tree_nodes();
        let Some(current_index) = visible
            .iter()
            .position(|node| node.node_id == self.selected_tree_node)
        else {
            self.selected_tree_node = 0;
            return;
        };

        let next_index = current_index.saturating_add_signed(delta);
        let clamped_index = next_index.min(visible.len().saturating_sub(1));
        self.selected_tree_node = visible[clamped_index].node_id;
    }

    pub fn collapse_or_select_parent(&mut self) {
        let node = &mut self.tree_nodes[self.selected_tree_node];
        if node.kind == NodeKind::Directory && node.expanded {
            node.expanded = false;
            return;
        }

        if let Some(parent) = node.parent {
            self.selected_tree_node = parent;
        }
    }

    pub fn expand_selected(&mut self) {
        let node = &mut self.tree_nodes[self.selected_tree_node];
        if node.kind == NodeKind::Directory {
            node.expanded = true;
        }
    }

    pub fn toggle_selected_tree_node(&mut self) {
        let enable = !self.tree_nodes[self.selected_tree_node].enabled;
        self.set_subtree_enabled(self.selected_tree_node, enable);
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Tree => FocusPane::Extensions,
            FocusPane::Extensions => FocusPane::Tree,
        };
    }

    pub fn move_extension_selection(&mut self, delta: isize) {
        if self.extension_filters.is_empty() {
            return;
        }
        let next_index = self.selected_extension.saturating_add_signed(delta);
        self.selected_extension = next_index.min(self.extension_filters.len() - 1);
    }

    pub fn toggle_selected_extension(&mut self) {
        if let Some(extension) = self.extension_filters.get_mut(self.selected_extension) {
            extension.enabled = !extension.enabled;
        }
    }

    pub fn selected_extension_label(&self) -> &str {
        self.extension_filters
            .get(self.selected_extension)
            .map(|filter| filter.extension.as_str())
            .unwrap_or("[none]")
    }

    fn bump_processed_counts(&mut self, node_id: usize, path: &BString) {
        let mut cursor = Some(node_id);
        while let Some(current) = cursor {
            self.tree_nodes[current].processed_files += 1;
            cursor = self.tree_nodes[current].parent;
        }

        let extension = extension_for_path(path);
        if let Some(filter) = self
            .extension_filters
            .iter_mut()
            .find(|filter| filter.extension == extension)
        {
            filter.processed_files += 1;
        }
    }

    fn push_recent_file(&mut self, recent_file: RecentFile) {
        if self.recent_files.len() == 16 {
            self.recent_files.pop_back();
        }
        self.recent_files.push_front(recent_file);
    }

    fn file_in_scope(&self, record: &FileRecord, scope: usize) -> bool {
        record.summary.is_some()
            && self.file_matches_view_filters(record)
            && self.is_descendant(record.node_id, scope)
    }

    fn file_matches_view_filters(&self, record: &FileRecord) -> bool {
        self.tree_nodes[record.node_id].enabled
            && self
                .extension_filters
                .iter()
                .find(|filter| filter.extension == record.extension)
                .map(|filter| filter.enabled)
                .unwrap_or(true)
    }

    fn is_descendant(&self, node_id: usize, ancestor_id: usize) -> bool {
        let mut cursor = Some(node_id);
        while let Some(current) = cursor {
            if current == ancestor_id {
                return true;
            }
            cursor = self.tree_nodes[current].parent;
        }
        false
    }

    fn set_subtree_enabled(&mut self, node_id: usize, enabled: bool) {
        self.tree_nodes[node_id].enabled = enabled;
        let children = self.tree_nodes[node_id].children.clone();
        for child_id in children {
            self.set_subtree_enabled(child_id, enabled);
        }
    }

    fn push_visible_tree_nodes(
        &self,
        node_id: usize,
        depth: usize,
        visible: &mut Vec<VisibleTreeNode>,
    ) {
        visible.push(VisibleTreeNode { node_id, depth });
        if self.tree_nodes[node_id].kind == NodeKind::Directory && self.tree_nodes[node_id].expanded
        {
            for child_id in &self.tree_nodes[node_id].children {
                self.push_visible_tree_nodes(*child_id, depth + 1, visible);
            }
        }
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
