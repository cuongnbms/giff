mod event_loop;
mod rebase;
pub(crate) mod render;
mod syntax;
pub(crate) mod theme;
mod types;

#[cfg(test)]
mod tests;

pub use types::ViewMode;

/// Initial UI state sourced from config: what view mode to open in and
/// whether word wrap starts on.
pub struct UiDefaults {
    pub view_mode: ViewMode,
    pub wrap_mode: bool,
}

use crate::diff::{self, DiffSource, FileChanges};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use ratatui::{prelude::*, Terminal};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::{error::Error, io};

use event_loop::run_ui;
use types::*;

/// Check if a syntax highlighting theme name is available in syntect.
pub fn is_valid_syntax_theme(name: &str) -> bool {
    syntax::THEME_SET.themes.contains_key(name)
}

/// Restore the terminal to its normal state. Best-effort: errors are ignored
/// because this is typically called during cleanup or panic recovery.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

pub fn run_app(
    file_changes: FileChanges,
    left_label: String,
    right_label: String,
    theme: theme::Theme,
    rebase_notification: Option<String>,
    diff_source: DiffSource,
    defaults: UiDefaults,
) -> Result<(), Box<dyn Error>> {
    // Install a panic hook that restores the terminal before printing the
    // panic message. Without this, a panic leaves the terminal in raw mode
    // with the alternate screen still active, making it unusable.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut file_names: Vec<String> = file_changes.keys().cloned().collect();
    file_names.sort();

    let mut scroll_positions = HashMap::new();
    let mut h_scroll_positions = HashMap::new();
    for name in &file_names {
        scroll_positions.insert(name.clone(), 0usize);
        h_scroll_positions.insert(name.clone(), 0usize);
    }

    // Build theme cycle: [initial, dark, light] with dedup
    let dark = theme::Theme::dark();
    let light = theme::Theme::light();
    let mut theme_cycle = vec![theme.clone()];
    if theme != dark {
        theme_cycle.push(dark);
    }
    if theme != light {
        theme_cycle.push(light);
    }

    let app = App {
        file_changes,
        left_label,
        right_label,
        current_file_idx: 0,
        file_names,
        scroll_positions,
        h_scroll_positions,
        focused_pane: Pane::FileList,
        view_mode: defaults.view_mode,
        app_mode: AppMode::Diff,
        rebase_changes: HashMap::new(),
        current_change_idx: 0,
        show_rebase_modal: rebase_notification.is_some(),
        rebase_notification,
        status_message: None,
        show_help_modal: false,
        theme,
        theme_cycle,
        theme_cycle_idx: 0,
        diff_source,
        commits: Vec::new(),
        current_commit_idx: 0,
        log_return_source: None,
        remotes: Vec::new(),
        current_remote_idx: 0,
        branch_status: diff::branch_status().ok(),
        file_list_width_pct: 20,
        resizing_divider: false,
        full_file: false,
        wrap_mode: defaults.wrap_mode,
        pending_commit_message: None,
        show_commit_modal: false,
    };

    // Watch the working tree for changes so we can auto-reload the diff.
    // The watcher is held in this scope so it stays alive for the UI's lifetime.
    let (reload_tx, reload_rx) = mpsc::channel::<()>();
    let _watcher = match diff::git_repo_root() {
        Ok(root) => spawn_repo_watcher(&root, reload_tx).ok(),
        Err(_) => None,
    };

    // Run the main loop
    let res = run_ui(&mut terminal, app, &reload_rx);

    // Restore terminal
    restore_terminal();
    terminal.show_cursor()?;

    match res {
        Ok(true) => {
            println!("Rebase completed successfully. Please re-run giff to see updated changes.");
        }
        Err(err) => {
            println!("{:?}", err);
        }
        _ => {}
    }

    Ok(())
}

/// Watch `repo_root` recursively. Filesystem events that look meaningful for
/// a git diff (working-tree edits, ref/index updates) are forwarded as a
/// single `()` ping per event into `tx`. Noisy paths (`.git/objects/`,
/// `.git/logs/`, `*.lock`) are ignored to avoid reload storms during git
/// operations.
fn spawn_repo_watcher(
    repo_root: &str,
    tx: mpsc::Sender<()>,
) -> Result<notify::RecommendedWatcher, Box<dyn Error>> {
    let repo_root_buf = std::path::PathBuf::from(repo_root);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let event = match res {
            Ok(e) => e,
            Err(_) => return,
        };

        // Only react to actual content/metadata changes.
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }

        if event
            .paths
            .iter()
            .all(|p| is_ignored_path(p, &repo_root_buf))
        {
            return;
        }

        let _ = tx.send(());
    })?;

    watcher.watch(Path::new(repo_root), RecursiveMode::Recursive)?;
    Ok(watcher)
}

fn is_ignored_path(path: &Path, repo_root: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.ends_with(".lock") {
            return true;
        }
    }

    let rel = match path.strip_prefix(repo_root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let rel_str = rel.to_string_lossy();
    rel_str.starts_with(".git/objects/")
        || rel_str.starts_with(".git/logs/")
        || rel_str.starts_with(".git/info/")
        || rel_str == ".git/FETCH_HEAD"
        || rel_str == ".git/ORIG_HEAD"
}
