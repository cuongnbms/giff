use crate::diff::{BranchStatus, CommitInfo, DiffSource, FileChanges, FileMetaMap, FullContent};
use std::cell::RefCell;
use std::collections::HashMap;

use super::syntax::HighlightCache;
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
    /// Full base/head text of each changed file (no diff markers), used to
    /// prime syntax-highlight parse state so multi-line constructs opened
    /// above a hunk are colored correctly. Refreshed on every diff (re)load;
    /// empty when unavailable, in which case highlighting falls back to the
    /// unprimed behavior.
    pub full_content: FullContent,
    /// Per-file metadata (rename info, etc.) parsed from the diff headers.
    /// Parallel to `file_changes`: same keys.
    pub file_meta: FileMetaMap,
    pub left_label: String,
    pub right_label: String,
    pub current_file_idx: usize,
    /// Names visible in the file list. Derived from `file_changes.keys()`
    /// filtered by `hide_pure_renames`. Never iterate `file_changes.keys()`
    /// directly for display — always read this.
    pub file_names: Vec<String>,
    pub scroll_positions: HashMap<String, usize>,
    pub h_scroll_positions: HashMap<String, usize>,
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
    /// Current branch + ahead/behind counts shown in the header.
    pub branch_status: Option<BranchStatus>,
    /// Width of the Files pane as a percentage of the terminal (Diff mode).
    /// Mutated by dragging the vertical divider with the mouse.
    pub file_list_width_pct: u16,
    /// True while the user is mid-drag on the Files/Diff divider.
    pub resizing_divider: bool,
    /// When true, the diff is fetched with `--unified=<huge>`, so every line
    /// of each changed file is shown as context (full-file view).
    pub full_file: bool,
    /// When true, long diff lines wrap onto multiple visual rows instead of
    /// being clipped (with horizontal scroll). Toggled with `w` in Diff mode.
    pub wrap_mode: bool,
    /// When true, files that git reports as 100% renames (no content change)
    /// are hidden from the file list. Toggled with `R` in Diff mode.
    pub hide_pure_renames: bool,
    /// When true, the file panel renders as an indented directory tree instead
    /// of the flat sorted list. Toggled with `T` in Diff mode.
    pub file_tree_view: bool,
    /// AI-generated commit message awaiting user confirmation. Shown via the
    /// commit modal; `None` means no commit flow is in progress.
    pub pending_commit_message: Option<String>,
    /// Whether the commit confirmation modal is currently displayed.
    pub show_commit_modal: bool,
    /// Per-render memoization of syntax-highlighted diff lines. Keyed by a
    /// content + theme hash so scrolling reuses the result instead of
    /// re-highlighting the whole file every frame. `RefCell` because the
    /// render path holds `&App`.
    pub highlight_cache: RefCell<HighlightCache>,
    /// Logical line index of the diff-pane cursor within the current view's
    /// line sequence (unified lines, or aligned side-by-side rows). Reset on
    /// file switch, view-mode toggle, and diff reload.
    pub diff_cursor: usize,
    /// When `Some`, a visual-line selection is active anchored at this logical
    /// line index; the selection spans `anchor..=diff_cursor`.
    pub selection_anchor: Option<usize>,
    /// True between mouse-down and mouse-up while drag-selecting in the diff
    /// pane, so `Drag` events extend the selection.
    pub mouse_selecting: bool,
}

impl App {
    /// Clear the diff-pane cursor and any active selection. Called whenever the
    /// rendered line sequence changes meaning (file switch, view toggle, reload).
    pub fn reset_diff_selection(&mut self) {
        self.diff_cursor = 0;
        self.selection_anchor = None;
        self.mouse_selecting = false;
    }

    /// The inclusive selection range `(lo, hi)` in logical line indices, or
    /// `None` when no selection is active.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.selection_anchor
            .map(|a| (a.min(self.diff_cursor), a.max(self.diff_cursor)))
    }
}

pub enum Pane {
    FileList,
    DiffContent,
}

pub enum ViewMode {
    SideBySide,
    Unified,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_app() -> App {
        App {
            file_changes: Default::default(),
            full_content: Default::default(),
            file_meta: Default::default(),
            left_label: String::new(),
            right_label: String::new(),
            current_file_idx: 0,
            file_names: Vec::new(),
            scroll_positions: Default::default(),
            h_scroll_positions: Default::default(),
            focused_pane: Pane::DiffContent,
            view_mode: ViewMode::Unified,
            app_mode: AppMode::Diff,
            rebase_changes: Default::default(),
            current_change_idx: 0,
            rebase_notification: None,
            show_rebase_modal: false,
            status_message: None,
            show_help_modal: false,
            theme: Theme::dark(),
            theme_cycle: Vec::new(),
            theme_cycle_idx: 0,
            diff_source: crate::diff::DiffSource::Uncommitted,
            commits: Vec::new(),
            current_commit_idx: 0,
            log_return_source: None,
            remotes: Vec::new(),
            current_remote_idx: 0,
            branch_status: None,
            file_list_width_pct: 30,
            resizing_divider: false,
            full_file: false,
            wrap_mode: false,
            hide_pure_renames: false,
            file_tree_view: false,
            pending_commit_message: None,
            show_commit_modal: false,
            highlight_cache: std::cell::RefCell::new(crate::ui::syntax::HighlightCache::default()),
            diff_cursor: 0,
            selection_anchor: None,
            mouse_selecting: false,
        }
    }

    #[test]
    fn selection_range_orders_anchor_and_cursor() {
        let mut app = bare_app();
        app.diff_cursor = 5;
        app.selection_anchor = Some(2);
        assert_eq!(app.selection_range(), Some((2, 5)));
        app.diff_cursor = 1;
        assert_eq!(app.selection_range(), Some((1, 2)));
        app.selection_anchor = None;
        assert_eq!(app.selection_range(), None);
    }

    #[test]
    fn reset_clears_cursor_and_selection() {
        let mut app = bare_app();
        app.diff_cursor = 9;
        app.selection_anchor = Some(3);
        app.mouse_selecting = true;
        app.reset_diff_selection();
        assert_eq!(app.diff_cursor, 0);
        assert_eq!(app.selection_anchor, None);
        assert!(!app.mouse_selecting);
    }
}
