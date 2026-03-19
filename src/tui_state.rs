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
