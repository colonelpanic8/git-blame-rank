use std::collections::HashMap;

use smol_str::SmolStr;

use crate::core::{AuthorRow, FileRecord, NodeKind, ScanState, display_path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusPane {
    Tree,
    Extensions,
    Rankings,
    Recent,
}

#[derive(Debug)]
pub struct TreeNodeState {
    pub expanded: bool,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct ExtensionFilterState {
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct VisibleTreeNode {
    pub node_id: usize,
    pub depth: usize,
}

#[derive(Debug)]
pub struct TuiState {
    pub tree_nodes: Vec<TreeNodeState>,
    pub selected_tree_node: usize,
    pub extension_filters: Vec<ExtensionFilterState>,
    pub selected_extension: usize,
    pub selected_author_row: usize,
    pub selected_recent_file: usize,
    pub focus: FocusPane,
    extension_index_by_name: HashMap<SmolStr, usize>,
}

impl TuiState {
    pub fn new(scan_state: &ScanState) -> Self {
        let tree_nodes = scan_state
            .tree_nodes
            .iter()
            .enumerate()
            .map(|(node_id, _)| TreeNodeState {
                expanded: node_id == 0,
                enabled: true,
            })
            .collect::<Vec<_>>();
        let extension_filters = scan_state
            .extensions
            .iter()
            .map(|_| ExtensionFilterState { enabled: true })
            .collect::<Vec<_>>();
        let extension_index_by_name = scan_state
            .extensions
            .iter()
            .enumerate()
            .map(|(index, stat)| (stat.extension.clone(), index))
            .collect();

        Self {
            tree_nodes,
            selected_tree_node: 0,
            extension_filters,
            selected_extension: 0,
            selected_author_row: 0,
            selected_recent_file: 0,
            focus: FocusPane::Tree,
            extension_index_by_name,
        }
    }

    pub fn author_rows(&self, scan_state: &ScanState) -> Vec<AuthorRow> {
        let selected_scope = self.selected_tree_node;
        scan_state
            .author_rows_filtered(|record| self.file_in_scope(record, scan_state, selected_scope))
    }

    pub fn current_scope_label(&self, scan_state: &ScanState) -> String {
        if self.selected_tree_node == 0 {
            ".".to_owned()
        } else {
            display_path(&scan_state.tree_nodes[self.selected_tree_node].path)
        }
    }

    pub fn current_scope_file_counts(&self, scan_state: &ScanState) -> (usize, usize) {
        let selected_scope = self.selected_tree_node;
        scan_state.file_counts_filtered(|record| {
            self.file_matches_view_filters(record)
                && self.is_descendant(scan_state, record.node_id, selected_scope)
        })
    }

    pub fn visible_tree_nodes(&self, scan_state: &ScanState) -> Vec<VisibleTreeNode> {
        let mut visible = Vec::new();
        self.push_visible_tree_nodes(scan_state, 0, 0, &mut visible);
        visible
    }

    pub fn move_tree_selection(&mut self, scan_state: &ScanState, delta: isize) {
        let visible = self.visible_tree_nodes(scan_state);
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

    pub fn collapse_or_select_parent(&mut self, scan_state: &ScanState) {
        let node = &scan_state.tree_nodes[self.selected_tree_node];
        let node_state = &mut self.tree_nodes[self.selected_tree_node];
        if node.kind == NodeKind::Directory && node_state.expanded {
            node_state.expanded = false;
            return;
        }

        if let Some(parent) = node.parent {
            self.selected_tree_node = parent;
        }
    }

    pub fn expand_selected(&mut self, scan_state: &ScanState) {
        let node = &scan_state.tree_nodes[self.selected_tree_node];
        if node.kind == NodeKind::Directory {
            self.tree_nodes[self.selected_tree_node].expanded = true;
        }
    }

    pub fn toggle_selected_tree_node(&mut self, scan_state: &ScanState) {
        let enable = !self.tree_nodes[self.selected_tree_node].enabled;
        self.set_subtree_enabled(scan_state, self.selected_tree_node, enable);
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Tree => FocusPane::Extensions,
            FocusPane::Extensions => FocusPane::Rankings,
            FocusPane::Rankings => FocusPane::Recent,
            FocusPane::Recent => FocusPane::Tree,
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
        if let Some(filter) = self.extension_filters.get_mut(self.selected_extension) {
            filter.enabled = !filter.enabled;
        }
    }

    pub fn move_author_selection(&mut self, scan_state: &ScanState, delta: isize) {
        let total = self.author_rows(scan_state).len();
        if total == 0 {
            self.selected_author_row = 0;
            return;
        }

        let next_index = self.selected_author_row.saturating_add_signed(delta);
        self.selected_author_row = next_index.min(total - 1);
    }

    pub fn move_recent_selection(&mut self, scan_state: &ScanState, delta: isize) {
        let total = scan_state.recent_files.len();
        if total == 0 {
            self.selected_recent_file = 0;
            return;
        }

        let next_index = self.selected_recent_file.saturating_add_signed(delta);
        self.selected_recent_file = next_index.min(total - 1);
    }

    pub fn selected_extension_label<'a>(&self, scan_state: &'a ScanState) -> &'a str {
        scan_state
            .extensions
            .get(self.selected_extension)
            .map(|filter| filter.extension.as_str())
            .unwrap_or("[none]")
    }

    fn file_in_scope(&self, record: &FileRecord, scan_state: &ScanState, scope: usize) -> bool {
        record.summary.is_some()
            && self.file_matches_view_filters(record)
            && self.is_descendant(scan_state, record.node_id, scope)
    }

    fn file_matches_view_filters(&self, record: &FileRecord) -> bool {
        self.tree_nodes[record.node_id].enabled
            && self
                .extension_index_by_name
                .get(&record.extension)
                .and_then(|index| self.extension_filters.get(*index))
                .map(|filter| filter.enabled)
                .unwrap_or(true)
    }

    fn is_descendant(&self, scan_state: &ScanState, node_id: usize, ancestor_id: usize) -> bool {
        let mut cursor = Some(node_id);
        while let Some(current) = cursor {
            if current == ancestor_id {
                return true;
            }
            cursor = scan_state.tree_nodes[current].parent;
        }
        false
    }

    fn set_subtree_enabled(&mut self, scan_state: &ScanState, node_id: usize, enabled: bool) {
        self.tree_nodes[node_id].enabled = enabled;
        let children = scan_state.tree_nodes[node_id].children.clone();
        for child_id in children {
            self.set_subtree_enabled(scan_state, child_id, enabled);
        }
    }

    fn push_visible_tree_nodes(
        &self,
        scan_state: &ScanState,
        node_id: usize,
        depth: usize,
        visible: &mut Vec<VisibleTreeNode>,
    ) {
        visible.push(VisibleTreeNode { node_id, depth });
        if scan_state.tree_nodes[node_id].kind == NodeKind::Directory
            && self.tree_nodes[node_id].expanded
        {
            for child_id in &scan_state.tree_nodes[node_id].children {
                self.push_visible_tree_nodes(scan_state, *child_id, depth + 1, visible);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use bstr::BString;
    use git2::Oid;

    use crate::core::{AuthorKey, FileAuthorStat, FileSummary};
    use crate::event::WorkerEvent;

    use super::*;

    fn oid(hex_digit: char) -> Oid {
        Oid::from_str(&hex_digit.to_string().repeat(40)).unwrap()
    }

    fn summary(path: &str, author_name: &str, author_email: &str, lines: u32) -> FileSummary {
        FileSummary {
            path: BString::from(path),
            total_lines: lines,
            authors: vec![FileAuthorStat {
                author: AuthorKey {
                    name: SmolStr::new(author_name),
                    email: SmolStr::new(author_email),
                },
                lines,
                commit_ids: vec![oid(author_name
                    .chars()
                    .next()
                    .unwrap()
                    .to_ascii_lowercase())],
            }],
        }
    }

    fn find_node_id(scan_state: &ScanState, path: &str) -> usize {
        scan_state
            .tree_nodes
            .iter()
            .position(|node| node.path == BString::from(path))
            .unwrap()
    }

    fn sample_scan_state() -> ScanState {
        let mut scan_state = ScanState::new(
            PathBuf::from("/repo"),
            "HEAD".to_owned(),
            2,
            vec![
                BString::from("README.md"),
                BString::from("src/lib.rs"),
                BString::from("src/main.rs"),
            ],
        );
        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: BString::from("README.md"),
            summary: summary("README.md", "Bob", "bob@example.com", 8),
            elapsed: Duration::default(),
        });
        scan_state.apply_worker_event(WorkerEvent::Finished {
            path: BString::from("src/lib.rs"),
            summary: summary("src/lib.rs", "Alice", "alice@example.com", 10),
            elapsed: Duration::default(),
        });
        scan_state.apply_worker_event(WorkerEvent::Failed {
            path: BString::from("src/main.rs"),
            error: "failed".into(),
            elapsed: Duration::default(),
        });
        scan_state
    }

    #[test]
    fn new_initializes_root_expanded_and_all_filters_enabled() {
        let scan_state = sample_scan_state();
        let tui_state = TuiState::new(&scan_state);

        assert!(tui_state.tree_nodes[0].expanded);
        assert!(tui_state.tree_nodes.iter().all(|node| node.enabled));
        assert!(
            tui_state
                .extension_filters
                .iter()
                .all(|filter| filter.enabled)
        );
        assert_eq!(tui_state.focus, FocusPane::Tree);
    }

    #[test]
    fn visible_tree_nodes_expand_and_collapse_with_navigation() {
        let scan_state = sample_scan_state();
        let mut tui_state = TuiState::new(&scan_state);
        let src_node = find_node_id(&scan_state, "src");

        let initial_visible = tui_state.visible_tree_nodes(&scan_state);
        assert_eq!(
            initial_visible
                .iter()
                .map(|node| node.node_id)
                .collect::<Vec<_>>(),
            vec![
                0,
                find_node_id(&scan_state, "src"),
                find_node_id(&scan_state, "README.md")
            ]
        );

        tui_state.move_tree_selection(&scan_state, 1);
        assert_eq!(tui_state.selected_tree_node, src_node);

        tui_state.expand_selected(&scan_state);
        let expanded_visible = tui_state.visible_tree_nodes(&scan_state);
        assert_eq!(
            expanded_visible
                .iter()
                .map(|node| (node.node_id, node.depth))
                .collect::<Vec<_>>(),
            vec![
                (0, 0),
                (src_node, 1),
                (find_node_id(&scan_state, "src/lib.rs"), 2),
                (find_node_id(&scan_state, "src/main.rs"), 2),
                (find_node_id(&scan_state, "README.md"), 1),
            ]
        );

        tui_state.move_tree_selection(&scan_state, 10);
        assert_eq!(
            tui_state.selected_tree_node,
            find_node_id(&scan_state, "README.md")
        );
        tui_state.move_tree_selection(&scan_state, -10);
        assert_eq!(tui_state.selected_tree_node, 0);

        tui_state.selected_tree_node = src_node;
        tui_state.collapse_or_select_parent(&scan_state);
        assert!(!tui_state.tree_nodes[src_node].expanded);
        tui_state.collapse_or_select_parent(&scan_state);
        assert_eq!(tui_state.selected_tree_node, 0);
    }

    #[test]
    fn author_rows_and_counts_respect_scope_and_extension_filters() {
        let scan_state = sample_scan_state();
        let mut tui_state = TuiState::new(&scan_state);
        let src_node = find_node_id(&scan_state, "src");
        tui_state.selected_tree_node = src_node;

        let rows = tui_state.author_rows(&scan_state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author.name.as_str(), "Alice");

        let counts = tui_state.current_scope_file_counts(&scan_state);
        assert_eq!(counts, (2, 2));
        assert_eq!(tui_state.current_scope_label(&scan_state), "src");

        tui_state.selected_extension = scan_state
            .extensions
            .iter()
            .position(|stat| stat.extension == "rs")
            .unwrap();
        tui_state.toggle_selected_extension();

        assert!(tui_state.author_rows(&scan_state).is_empty());
        assert_eq!(tui_state.current_scope_file_counts(&scan_state), (0, 0));
    }

    #[test]
    fn toggling_tree_node_disables_entire_subtree() {
        let scan_state = sample_scan_state();
        let mut tui_state = TuiState::new(&scan_state);
        let src_node = find_node_id(&scan_state, "src");
        let src_lib = find_node_id(&scan_state, "src/lib.rs");
        let src_main = find_node_id(&scan_state, "src/main.rs");

        tui_state.selected_tree_node = src_node;
        tui_state.toggle_selected_tree_node(&scan_state);

        assert!(!tui_state.tree_nodes[src_node].enabled);
        assert!(!tui_state.tree_nodes[src_lib].enabled);
        assert!(!tui_state.tree_nodes[src_main].enabled);

        tui_state.selected_tree_node = 0;
        let rows = tui_state.author_rows(&scan_state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author.name.as_str(), "Bob");
    }

    #[test]
    fn selection_helpers_and_focus_cycle_clamp_safely() {
        let scan_state = sample_scan_state();
        let mut tui_state = TuiState::new(&scan_state);

        tui_state.move_extension_selection(50);
        assert_eq!(
            tui_state.selected_extension,
            scan_state.extensions.len() - 1
        );
        tui_state.move_extension_selection(-50);
        assert_eq!(tui_state.selected_extension, 0);

        tui_state.move_author_selection(&scan_state, 50);
        assert_eq!(tui_state.selected_author_row, 1);
        tui_state.move_author_selection(&scan_state, -50);
        assert_eq!(tui_state.selected_author_row, 0);

        tui_state.move_recent_selection(&scan_state, 50);
        assert_eq!(
            tui_state.selected_recent_file,
            scan_state.recent_files.len() - 1
        );
        tui_state.move_recent_selection(&scan_state, -50);
        assert_eq!(tui_state.selected_recent_file, 0);

        tui_state.cycle_focus();
        assert_eq!(tui_state.focus, FocusPane::Extensions);
        tui_state.cycle_focus();
        assert_eq!(tui_state.focus, FocusPane::Rankings);
        tui_state.cycle_focus();
        assert_eq!(tui_state.focus, FocusPane::Recent);
        tui_state.cycle_focus();
        assert_eq!(tui_state.focus, FocusPane::Tree);
    }
}
