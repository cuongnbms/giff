use crate::diff::{CommitInfo, DiffSource, FileChanges};
use std::collections::HashMap;

use super::theme::Theme;

pub enum AppMode {
    Diff,
    Rebase,
    Log,
    RemotePicker,
}

#[derive(Clone, PartialEq)]
pub enum ChangeState {
    Unselected,
    Accepted,
    Rejected,
}

#[derive(Clone, PartialEq)]
pub struct Change {
    pub line_num: usize,
    pub content: String,
    pub paired_content: Option<String>, // The paired line (if any)
    pub state: ChangeState,
    pub is_base: bool,
    pub context: Vec<String>,
    /// For unpaired additions: the computed base-file position to insert at.
    pub base_insert_pos: Option<usize>,
}

pub struct App {
    pub file_changes: FileChanges,
    pub left_label: String,
    pub right_label: String,
    pub current_file_idx: usize,
    pub file_names: Vec<String>,
    pub scroll_positions: HashMap<String, usize>,
    pub focused_pane: Pane,
    pub view_mode: ViewMode,
    pub app_mode: AppMode,
    pub rebase_changes: HashMap<String, Vec<Change>>,
    pub current_change_idx: usize,
    pub rebase_notification: Option<String>,
    pub show_rebase_modal: bool,
    /// Transient status message shown in the help bar (cleared on next keypress)
    pub status_message: Option<String>,
    pub show_help_modal: bool,
    pub theme: Theme,
    pub theme_cycle: Vec<Theme>,
    pub theme_cycle_idx: usize,
    /// How to refresh the diff when the working tree changes.
    pub diff_source: DiffSource,
    /// Commits loaded for log view (lazy-loaded on first entry to Log mode).
    pub commits: Vec<CommitInfo>,
    pub current_commit_idx: usize,
    /// The diff source to restore when leaving Log mode (Esc/q).
    /// Set when first entering Log mode; cleared when leaving.
    pub log_return_source: Option<DiffSource>,
    /// Configured remotes shown in the picker. Empty unless `app_mode == RemotePicker`.
    pub remotes: Vec<String>,
    pub current_remote_idx: usize,
}

pub enum Pane {
    FileList,
    DiffContent,
}

pub enum ViewMode {
    SideBySide,
    Unified,
}
