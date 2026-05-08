use crate::diff::{self, DiffSource};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{prelude::*, Terminal};
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::rebase::prepare_rebase_changes;
use super::render::ui;
use super::types::*;

#[derive(Debug, PartialEq)]
enum PushDecision {
    NoRemotes,
    Single(String),
    NeedsPicker(Vec<String>),
}

fn decide_push_target(mut remotes: Vec<String>) -> PushDecision {
    match remotes.len() {
        0 => PushDecision::NoRemotes,
        1 => PushDecision::Single(remotes.remove(0)),
        _ => PushDecision::NeedsPicker(remotes),
    }
}

fn commit_rebase_changes(app: &mut App) {
    let mut any_applied = false;
    let mut errors = Vec::new();

    for (file, changes) in &app.rebase_changes {
        let mut operations = Vec::new();

        for change in changes {
            if change.state != ChangeState::Accepted {
                continue;
            }

            if change.is_base {
                if let Some(paired_content) = &change.paired_content {
                    // Replace: swap old content with incoming content
                    let clean = paired_content.strip_prefix('+').unwrap_or(paired_content);
                    operations.push(diff::ChangeOp::Replace(change.line_num, clean.to_string()));
                } else {
                    // Delete: remove the line entirely
                    operations.push(diff::ChangeOp::Delete(change.line_num));
                }
            } else {
                // Insert: use computed base position
                let clean = change.content.strip_prefix('+').unwrap_or(&change.content);
                let base_pos = change.base_insert_pos.unwrap_or(change.line_num);
                operations.push(diff::ChangeOp::Insert {
                    base_pos,
                    order: change.line_num,
                    content: clean.to_string(),
                });
            }
        }

        if !operations.is_empty() {
            any_applied = true;
            if let Err(e) = diff::apply_changes(file, &operations) {
                errors.push(format!("{}: {}", file, e));
            }
        }
    }

    // Surface feedback through the UI
    if !errors.is_empty() {
        app.status_message = Some(format!("Error: {}", errors.join("; ")));
    } else if any_applied {
        app.status_message = Some("Changes applied successfully!".to_string());
    } else {
        app.status_message = Some("No accepted changes to apply.".to_string());
    }

    // Return to diff mode
    app.app_mode = AppMode::Diff;
}

fn set_change_state(app: &mut App, state: ChangeState) {
    if let Some(file) = app.file_names.get(app.current_file_idx) {
        if let Some(changes) = app.rebase_changes.get_mut(file) {
            if app.current_change_idx < changes.len() {
                changes[app.current_change_idx].state = state;
                if app.current_change_idx < changes.len() - 1 {
                    app.current_change_idx += 1;
                }
            }
        }
    }
}

/// Re-run the diff using `app.diff_source` and merge the new state into `app`,
/// preserving the user's current file selection and scroll positions where
/// possible. No-op (silent) if the new diff is identical to the current one.
fn reload_diff(app: &mut App) {
    // Don't reload while the user is mid-rebase or browsing the commit log;
    // either would invalidate the user's current selection state.
    if matches!(app.app_mode, AppMode::Rebase | AppMode::Log | AppMode::RemotePicker) {
        return;
    }

    let (new_changes, new_left, new_right) = match app.diff_source.fetch() {
        Ok(v) => v,
        Err(e) => {
            app.status_message = Some(format!("Reload failed: {}", e));
            return;
        }
    };

    if new_changes == app.file_changes
        && new_left == app.left_label
        && new_right == app.right_label
    {
        return;
    }

    let prev_selected = app.file_names.get(app.current_file_idx).cloned();

    let mut new_names: Vec<String> = new_changes.keys().cloned().collect();
    new_names.sort();

    // Drop scroll positions for files that no longer exist; keep the rest.
    app.scroll_positions
        .retain(|name, _| new_changes.contains_key(name));
    for name in &new_names {
        app.scroll_positions.entry(name.clone()).or_insert(0);
    }

    app.current_file_idx = match prev_selected {
        Some(name) => new_names
            .iter()
            .position(|n| n == &name)
            .unwrap_or(0),
        None => 0,
    };

    app.file_changes = new_changes;
    app.file_names = new_names;
    app.left_label = new_left;
    app.right_label = new_right;
    app.status_message = Some("Diff reloaded".to_string());
}

/// Replace `app`'s diff state from the given source. Resets file selection
/// and scroll positions to a clean slate. Used when switching to a commit's
/// diff or restoring the original diff source on log exit.
fn load_diff_from_source(app: &mut App, source: DiffSource) -> Result<(), String> {
    let (changes, left, right) = source.fetch().map_err(|e| e.to_string())?;
    let mut names: Vec<String> = changes.keys().cloned().collect();
    names.sort();

    app.scroll_positions.clear();
    for n in &names {
        app.scroll_positions.insert(n.clone(), 0);
    }
    app.file_changes = changes;
    app.file_names = names;
    app.left_label = left;
    app.right_label = right;
    app.current_file_idx = 0;
    app.diff_source = source;
    Ok(())
}

fn perform_sync(app: &mut App) {
    match diff::has_uncommitted_changes() {
        Ok(true) => {
            app.status_message = Some("Cannot sync: uncommitted changes".to_string());
            return;
        }
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
            return;
        }
        Ok(false) => {}
    }

    match diff::get_upstream_branch() {
        Ok(Some(upstream)) => sync_with_upstream(app, &upstream),
        Ok(None) => sync_without_upstream(app),
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
        }
    }
}

fn sync_with_upstream(app: &mut App, upstream: &str) {
    if let Err(e) = diff::pull_rebase() {
        app.status_message = Some(format!("Pull failed: {}", e));
        return;
    }
    if let Err(e) = diff::push() {
        app.status_message = Some(format!("Push failed: {}", e));
        return;
    }
    app.status_message = Some(format!("Synced with {}", upstream));
    reload_diff(app);
}

fn sync_without_upstream(app: &mut App) {
    let remotes = match diff::list_remotes() {
        Ok(r) => r,
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
            return;
        }
    };

    let branch = match diff::current_branch() {
        Ok(b) => b,
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
            return;
        }
    };

    match decide_push_target(remotes) {
        PushDecision::NoRemotes => {
            app.status_message = Some("No remotes configured".to_string());
        }
        PushDecision::Single(remote) => {
            push_to_remote(app, &remote, &branch);
        }
        PushDecision::NeedsPicker(list) => {
            app.remotes = list;
            app.current_remote_idx = 0;
            app.app_mode = AppMode::RemotePicker;
        }
    }
}

fn push_to_remote(app: &mut App, remote: &str, branch: &str) {
    match diff::push_set_upstream(remote, branch) {
        Ok(()) => {
            app.status_message = Some(format!(
                "Pushed {} \u{2192} {}/{}",
                branch, remote, branch
            ));
            reload_diff(app);
        }
        Err(e) => {
            app.status_message = Some(format!("Push failed: {}", e));
        }
    }
}

fn picker_navigate(app: &mut App, forward: bool) {
    if app.remotes.is_empty() {
        return;
    }
    let last = app.remotes.len() - 1;
    if forward {
        if app.current_remote_idx < last {
            app.current_remote_idx += 1;
        }
    } else {
        app.current_remote_idx = app.current_remote_idx.saturating_sub(1);
    }
}

fn picker_confirm(app: &mut App) {
    let branch = match diff::current_branch() {
        Ok(b) => b,
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
            picker_close(app);
            return;
        }
    };
    let remote = match app.remotes.get(app.current_remote_idx) {
        Some(r) => r.clone(),
        None => {
            picker_close(app);
            return;
        }
    };
    picker_close(app);
    push_to_remote(app, &remote, &branch);
}

fn picker_cancel(app: &mut App) {
    picker_close(app);
    app.status_message = Some("Sync cancelled".to_string());
}

fn picker_close(app: &mut App) {
    app.remotes.clear();
    app.current_remote_idx = 0;
    app.app_mode = AppMode::Diff;
}

fn enter_log_mode(app: &mut App) {
    let commits = match diff::get_commit_log() {
        Ok(c) => c,
        Err(e) => {
            app.status_message = Some(format!("git log failed: {}", e));
            return;
        }
    };
    if commits.is_empty() {
        app.status_message = Some("No commits found".to_string());
        return;
    }
    // Preserve the original diff source so Esc returns the user there.
    // Only set on first entry — re-entering from a commit's diff keeps the
    // truly original source intact.
    if app.log_return_source.is_none() {
        app.log_return_source = Some(app.diff_source.clone());
    }
    // Keep the commit selection across re-entries if the list hasn't changed.
    if app.commits.is_empty() {
        app.commits = commits;
        app.current_commit_idx = 0;
    }
    app.app_mode = AppMode::Log;
}

fn exit_log_mode(app: &mut App) {
    if let Some(src) = app.log_return_source.take() {
        if let Err(e) = load_diff_from_source(app, src) {
            app.status_message = Some(format!("Reload failed: {}", e));
        }
    }
    app.commits.clear();
    app.current_commit_idx = 0;
    app.app_mode = AppMode::Diff;
    app.focused_pane = Pane::FileList;
}

fn open_selected_commit(app: &mut App) {
    let (hash, subject) = match app.commits.get(app.current_commit_idx) {
        Some(c) => (c.hash.clone(), c.subject.clone()),
        None => return,
    };
    let source = DiffSource::Commit(hash.clone());
    if let Err(e) = load_diff_from_source(app, source) {
        app.status_message = Some(format!("Failed to load commit: {}", e));
        return;
    }
    app.app_mode = AppMode::Diff;
    app.focused_pane = Pane::FileList;
    app.status_message = Some(format!(
        "Viewing {} \u{2014} {} \u{2502} L: log  Esc: back",
        hash, subject
    ));
}

fn navigate_rebase_file(app: &mut App, forward: bool) {
    let len = app.file_names.len();
    if len == 0 {
        return;
    }
    for offset in 1..len {
        let idx = if forward {
            (app.current_file_idx + offset) % len
        } else {
            (app.current_file_idx + len - offset) % len
        };
        if let Some(changes) = app.rebase_changes.get(&app.file_names[idx]) {
            if !changes.is_empty() {
                app.current_file_idx = idx;
                app.current_change_idx = 0;
                return;
            }
        }
    }
}

/// Returns `Ok(true)` when the app exits after a successful rebase
/// (so the caller can print a message), `Ok(false)` for normal exit.
pub fn run_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    reload_rx: &mpsc::Receiver<()>,
) -> io::Result<bool>
where
    std::io::Error: From<B::Error>,
{
    // Coalesce reload signals so a burst of fs events triggers a single re-fetch.
    const RELOAD_DEBOUNCE: Duration = Duration::from_millis(150);
    let mut pending_reload_since: Option<Instant> = None;
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            terminal.draw(|f| ui(f, &mut app))?;
            needs_redraw = false;
        }

        // Drain any reload pings that arrived since the last iteration.
        let mut got_reload = false;
        while reload_rx.try_recv().is_ok() {
            got_reload = true;
        }
        if got_reload {
            pending_reload_since = Some(Instant::now());
        }
        if let Some(since) = pending_reload_since {
            if since.elapsed() >= RELOAD_DEBOUNCE {
                reload_diff(&mut app);
                pending_reload_since = None;
                needs_redraw = true;
            }
        }

        // When a reload is pending, sleep up to the remaining debounce window so
        // we wake exactly when it elapses. When idle, block indefinitely on
        // crossterm input (the watcher thread will deliver fs pings on its own
        // schedule, picked up at the next user input).
        let poll_timeout = match pending_reload_since {
            Some(since) => RELOAD_DEBOUNCE
                .checked_sub(since.elapsed())
                .unwrap_or(Duration::ZERO),
            None => Duration::from_millis(250),
        };

        if !event::poll(poll_timeout)? {
            continue;
        }
        let first = event::read()?;
        let mut events = vec![first];
        while event::poll(Duration::ZERO)? {
            events.push(event::read()?);
        }
        needs_redraw = true;

        for ev in events {
            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Clear transient status message on any keypress
                    app.status_message = None;

                    // Handle help modal if shown
                    if app.show_help_modal {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                                app.show_help_modal = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle rebase modal if shown
                    if app.show_rebase_modal {
                        match key.code {
                            KeyCode::Char('r') => match diff::get_upstream_branch() {
                                Ok(Some(upstream)) => match diff::perform_rebase(&upstream) {
                                    Ok(true) => {
                                        app.show_rebase_modal = false;
                                        return Ok(true);
                                    }
                                    Ok(false) => {
                                        app.rebase_notification = Some(
                                            "Rebase failed due to conflicts and was rolled back."
                                                .to_string(),
                                        );
                                    }
                                    Err(e) => {
                                        app.show_rebase_modal = false;
                                        app.status_message = Some(format!("Error: {}", e));
                                    }
                                },
                                Ok(None) => {
                                    app.show_rebase_modal = false;
                                    app.status_message =
                                        Some("No upstream branch configured.".to_string());
                                }
                                Err(e) => {
                                    app.show_rebase_modal = false;
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                            },
                            KeyCode::Char('i') | KeyCode::Esc => {
                                app.show_rebase_modal = false;
                            }
                            _ => {}
                        }
                        continue; // Skip other key processing when modal is shown
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            match app.app_mode {
                                AppMode::Diff => {
                                    // If the user drilled into a commit via the log,
                                    // back out to the original diff first; only quit
                                    // when there's nothing left to back out of.
                                    if app.log_return_source.is_some() {
                                        exit_log_mode(&mut app);
                                    } else {
                                        return Ok(false);
                                    }
                                }
                                AppMode::Rebase => {
                                    // Return to diff mode without applying changes
                                    app.app_mode = AppMode::Diff;
                                }
                                AppMode::Log => {
                                    exit_log_mode(&mut app);
                                }
                                AppMode::RemotePicker => {
                                    picker_cancel(&mut app);
                                }
                            }
                        }
                        KeyCode::Char('L') => match app.app_mode {
                            AppMode::Diff => enter_log_mode(&mut app),
                            AppMode::Log => exit_log_mode(&mut app),
                            AppMode::Rebase => {}
                            AppMode::RemotePicker => {}
                        },
                        KeyCode::Enter => match app.app_mode {
                            AppMode::Log => open_selected_commit(&mut app),
                            AppMode::RemotePicker => picker_confirm(&mut app),
                            _ => {}
                        },
                        KeyCode::Char('r') => {
                            if let AppMode::Diff = app.app_mode {
                                app.app_mode = AppMode::Rebase;
                                prepare_rebase_changes(&mut app);
                            }
                        }
                        KeyCode::Char('s') => {
                            if let AppMode::Diff = app.app_mode {
                                perform_sync(&mut app);
                            }
                        }
                        KeyCode::Char('a') => {
                            if let AppMode::Rebase = app.app_mode {
                                set_change_state(&mut app, ChangeState::Accepted);
                            }
                        }
                        KeyCode::Char('x') => {
                            if let AppMode::Rebase = app.app_mode {
                                set_change_state(&mut app, ChangeState::Rejected);
                            }
                        }
                        KeyCode::Char('c') => {
                            if let AppMode::Rebase = app.app_mode {
                                commit_rebase_changes(&mut app);
                            }
                        }
                        KeyCode::Char('j') | KeyCode::Down => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    if app.current_file_idx + 1 < app.file_names.len() {
                                        app.current_file_idx += 1;
                                    }
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        let scroll = *app.scroll_positions.get(file).unwrap_or(&0);
                                        app.scroll_positions.insert(file.clone(), scroll + 1);
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                if let Some(file) = app.file_names.get(app.current_file_idx) {
                                    if let Some(changes) = app.rebase_changes.get(file) {
                                        if !changes.is_empty()
                                            && app.current_change_idx < changes.len() - 1
                                        {
                                            app.current_change_idx += 1;
                                        }
                                    }
                                }
                            }
                            AppMode::Log => {
                                if app.current_commit_idx + 1 < app.commits.len() {
                                    app.current_commit_idx += 1;
                                }
                            }
                            AppMode::RemotePicker => picker_navigate(&mut app, true),
                        },
                        KeyCode::Char('k') | KeyCode::Up => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    if app.current_file_idx > 0 {
                                        app.current_file_idx -= 1;
                                    }
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        let scroll = *app.scroll_positions.get(file).unwrap_or(&0);
                                        if scroll > 0 {
                                            app.scroll_positions.insert(file.clone(), scroll - 1);
                                        }
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                if app.current_change_idx > 0 {
                                    app.current_change_idx -= 1;
                                }
                            }
                            AppMode::Log => {
                                if app.current_commit_idx > 0 {
                                    app.current_commit_idx -= 1;
                                }
                            }
                            AppMode::RemotePicker => picker_navigate(&mut app, false),
                        },
                        KeyCode::PageDown => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    let page = terminal.size()?.height.saturating_sub(6) as usize;
                                    app.current_file_idx = (app.current_file_idx + page)
                                        .min(app.file_names.len().saturating_sub(1));
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        let scroll = *app.scroll_positions.get(file).unwrap_or(&0);
                                        let page =
                                            terminal.size()?.height.saturating_sub(6) as usize;
                                        app.scroll_positions
                                            .insert(file.clone(), scroll.saturating_add(page));
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                if let Some(file) = app.file_names.get(app.current_file_idx) {
                                    if let Some(changes) = app.rebase_changes.get(file) {
                                        if !changes.is_empty() {
                                            let page =
                                                terminal.size()?.height.saturating_sub(6) as usize;
                                            app.current_change_idx = (app.current_change_idx
                                                + page)
                                                .min(changes.len() - 1);
                                        }
                                    }
                                }
                            }
                            AppMode::Log => {
                                if !app.commits.is_empty() {
                                    let page = terminal.size()?.height.saturating_sub(6) as usize;
                                    app.current_commit_idx = (app.current_commit_idx + page)
                                        .min(app.commits.len() - 1);
                                }
                            }
                            AppMode::RemotePicker => {}
                        },
                        KeyCode::PageUp => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    let page = terminal.size()?.height.saturating_sub(6) as usize;
                                    app.current_file_idx =
                                        app.current_file_idx.saturating_sub(page);
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        let scroll = *app.scroll_positions.get(file).unwrap_or(&0);
                                        let page =
                                            terminal.size()?.height.saturating_sub(6) as usize;
                                        app.scroll_positions
                                            .insert(file.clone(), scroll.saturating_sub(page));
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                let page = terminal.size()?.height.saturating_sub(6) as usize;
                                app.current_change_idx =
                                    app.current_change_idx.saturating_sub(page);
                            }
                            AppMode::Log => {
                                let page = terminal.size()?.height.saturating_sub(6) as usize;
                                app.current_commit_idx =
                                    app.current_commit_idx.saturating_sub(page);
                            }
                            AppMode::RemotePicker => {}
                        },
                        KeyCode::Home => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    app.current_file_idx = 0;
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        app.scroll_positions.insert(file.clone(), 0);
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                app.current_change_idx = 0;
                            }
                            AppMode::Log => {
                                app.current_commit_idx = 0;
                            }
                            AppMode::RemotePicker => {}
                        },
                        KeyCode::End => match app.app_mode {
                            AppMode::Diff => match app.focused_pane {
                                Pane::FileList => {
                                    app.current_file_idx = app.file_names.len().saturating_sub(1);
                                }
                                Pane::DiffContent => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        app.scroll_positions.insert(file.clone(), usize::MAX);
                                    }
                                }
                            },
                            AppMode::Rebase => {
                                if let Some(file) = app.file_names.get(app.current_file_idx) {
                                    if let Some(changes) = app.rebase_changes.get(file) {
                                        if !changes.is_empty() {
                                            app.current_change_idx = changes.len() - 1;
                                        }
                                    }
                                }
                            }
                            AppMode::Log => {
                                app.current_commit_idx =
                                    app.commits.len().saturating_sub(1);
                            }
                            AppMode::RemotePicker => {}
                        },
                        KeyCode::Tab => {
                            // Toggle between file list and diff content (only in diff mode)
                            if let AppMode::Diff = app.app_mode {
                                app.focused_pane = match app.focused_pane {
                                    Pane::FileList => Pane::DiffContent,
                                    Pane::DiffContent => Pane::FileList,
                                }
                            }
                        }
                        KeyCode::Char('h') | KeyCode::Left => {
                            if let AppMode::Diff = app.app_mode {
                                app.focused_pane = Pane::FileList;
                            }
                        }
                        KeyCode::Char('l') | KeyCode::Right => {
                            if let AppMode::Diff = app.app_mode {
                                app.focused_pane = Pane::DiffContent;
                            }
                        }
                        KeyCode::Char('t') => {
                            // Cycle through available themes
                            if !app.theme_cycle.is_empty() {
                                app.theme_cycle_idx =
                                    (app.theme_cycle_idx + 1) % app.theme_cycle.len();
                                app.theme = app.theme_cycle[app.theme_cycle_idx].clone();
                            }
                        }
                        KeyCode::Char('u') => {
                            // Toggle between unified and side-by-side view (only in diff mode)
                            if let AppMode::Diff = app.app_mode {
                                app.view_mode = match app.view_mode {
                                    ViewMode::SideBySide => ViewMode::Unified,
                                    ViewMode::Unified => ViewMode::SideBySide,
                                }
                            }
                        }
                        KeyCode::Char('n') => {
                            if let AppMode::Rebase = app.app_mode {
                                navigate_rebase_file(&mut app, true);
                            }
                        }
                        KeyCode::Char('p') => {
                            if let AppMode::Rebase = app.app_mode {
                                navigate_rebase_file(&mut app, false);
                            }
                        }
                        KeyCode::Char('?') => {
                            app.show_help_modal = true;
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    if app.show_help_modal || app.show_rebase_modal {
                        continue;
                    }
                    let size = terminal.size()?;
                    let scroll_amount: usize = 3;
                    match mouse.kind {
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            if mouse.row == 0 || mouse.row >= size.height.saturating_sub(1) {
                                continue;
                            }
                            let is_down = matches!(mouse.kind, MouseEventKind::ScrollDown);
                            match app.app_mode {
                                AppMode::Diff => {
                                    let file_list_width = size.width / 5;
                                    if mouse.column < file_list_width {
                                        if !app.file_names.is_empty() {
                                            if is_down {
                                                app.current_file_idx = (app.current_file_idx
                                                    + scroll_amount)
                                                    .min(app.file_names.len() - 1);
                                            } else {
                                                app.current_file_idx = app
                                                    .current_file_idx
                                                    .saturating_sub(scroll_amount);
                                            }
                                        }
                                    } else if let Some(file) =
                                        app.file_names.get(app.current_file_idx)
                                    {
                                        let scroll = *app.scroll_positions.get(file).unwrap_or(&0);
                                        let new_scroll = if is_down {
                                            scroll.saturating_add(scroll_amount)
                                        } else {
                                            scroll.saturating_sub(scroll_amount)
                                        };
                                        app.scroll_positions.insert(file.clone(), new_scroll);
                                    }
                                }
                                AppMode::Rebase => {
                                    if let Some(file) = app.file_names.get(app.current_file_idx) {
                                        if let Some(changes) = app.rebase_changes.get(file) {
                                            if !changes.is_empty() {
                                                if is_down {
                                                    app.current_change_idx =
                                                        (app.current_change_idx + scroll_amount)
                                                            .min(changes.len() - 1);
                                                } else {
                                                    app.current_change_idx = app
                                                        .current_change_idx
                                                        .saturating_sub(scroll_amount);
                                                }
                                            }
                                        }
                                    }
                                }
                                AppMode::Log => {
                                    if !app.commits.is_empty() {
                                        if is_down {
                                            app.current_commit_idx =
                                                (app.current_commit_idx + scroll_amount)
                                                    .min(app.commits.len() - 1);
                                        } else {
                                            app.current_commit_idx = app
                                                .current_commit_idx
                                                .saturating_sub(scroll_amount);
                                        }
                                    }
                                }
                                AppMode::RemotePicker => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        } // end event batch
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::Theme;
    use super::*;
    use crate::diff::DiffSource;
    use std::collections::HashMap;

    fn make_app(file_names: Vec<&str>, changes_for: Vec<&str>) -> App {
        let file_names: Vec<String> = file_names.into_iter().map(|s| s.to_string()).collect();
        let mut rebase_changes = HashMap::new();
        for name in &file_names {
            let changes = if changes_for.contains(&name.as_str()) {
                vec![Change {
                    line_num: 1,
                    content: "-old".to_string(),
                    paired_content: None,
                    state: ChangeState::Unselected,
                    is_base: true,
                    context: vec![],
                    base_insert_pos: None,
                }]
            } else {
                vec![]
            };
            rebase_changes.insert(name.clone(), changes);
        }
        App {
            file_changes: HashMap::new(),
            left_label: String::new(),
            right_label: String::new(),
            current_file_idx: 0,
            file_names,
            scroll_positions: HashMap::new(),
            focused_pane: Pane::FileList,
            view_mode: ViewMode::SideBySide,
            app_mode: AppMode::Rebase,
            rebase_changes,
            current_change_idx: 0,
            rebase_notification: None,
            show_rebase_modal: false,
            status_message: None,
            show_help_modal: false,
            theme: Theme::dark(),
            theme_cycle: vec![Theme::dark(), Theme::light()],
            theme_cycle_idx: 0,
            diff_source: DiffSource::Uncommitted,
            commits: Vec::new(),
            current_commit_idx: 0,
            log_return_source: None,
            remotes: Vec::new(),
            current_remote_idx: 0,
        }
    }

    #[test]
    fn navigate_forward_finds_next_file_with_changes() {
        let mut app = make_app(vec!["a.rs", "b.rs", "c.rs"], vec!["a.rs", "c.rs"]);
        app.current_file_idx = 0;
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 2); // skips b.rs (empty)
    }

    #[test]
    fn navigate_forward_wraps_around() {
        let mut app = make_app(vec!["a.rs", "b.rs", "c.rs"], vec!["a.rs", "c.rs"]);
        app.current_file_idx = 2;
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 0); // wraps to a.rs
    }

    #[test]
    fn navigate_backward_finds_previous_file_with_changes() {
        let mut app = make_app(vec!["a.rs", "b.rs", "c.rs"], vec!["a.rs", "c.rs"]);
        app.current_file_idx = 2;
        navigate_rebase_file(&mut app, false);
        assert_eq!(app.current_file_idx, 0); // skips b.rs
    }

    #[test]
    fn navigate_backward_wraps_around() {
        let mut app = make_app(vec!["a.rs", "b.rs", "c.rs"], vec!["a.rs", "c.rs"]);
        app.current_file_idx = 0;
        navigate_rebase_file(&mut app, false);
        assert_eq!(app.current_file_idx, 2); // wraps to c.rs
    }

    #[test]
    fn navigate_no_files_with_changes_stays_put() {
        let mut app = make_app(vec!["a.rs", "b.rs"], vec![]);
        app.current_file_idx = 0;
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 0); // unchanged
    }

    #[test]
    fn navigate_single_file_with_changes_stays_put() {
        let mut app = make_app(vec!["a.rs", "b.rs"], vec!["a.rs"]);
        app.current_file_idx = 0;
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 0); // only file with changes
    }

    #[test]
    fn navigate_empty_file_list() {
        let mut app = make_app(vec![], vec![]);
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 0);
    }

    #[test]
    fn navigate_resets_change_idx() {
        let mut app = make_app(vec!["a.rs", "b.rs"], vec!["a.rs", "b.rs"]);
        app.current_file_idx = 0;
        app.current_change_idx = 5;
        navigate_rebase_file(&mut app, true);
        assert_eq!(app.current_file_idx, 1);
        assert_eq!(app.current_change_idx, 0);
    }

    #[test]
    fn decide_push_target_no_remotes() {
        assert_eq!(decide_push_target(Vec::new()), PushDecision::NoRemotes);
    }

    #[test]
    fn decide_push_target_single_remote() {
        assert_eq!(
            decide_push_target(vec!["origin".to_string()]),
            PushDecision::Single("origin".to_string())
        );
    }

    #[test]
    fn decide_push_target_multiple_remotes() {
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        assert_eq!(
            decide_push_target(remotes.clone()),
            PushDecision::NeedsPicker(remotes)
        );
    }

    fn make_picker_app(remotes: Vec<&str>) -> App {
        let mut app = make_app(vec![], vec![]);
        app.app_mode = AppMode::RemotePicker;
        app.remotes = remotes.into_iter().map(|s| s.to_string()).collect();
        app.current_remote_idx = 0;
        app
    }

    #[test]
    fn picker_j_advances_within_bounds() {
        let mut app = make_picker_app(vec!["origin", "upstream"]);
        picker_navigate(&mut app, true);
        assert_eq!(app.current_remote_idx, 1);
    }

    #[test]
    fn picker_j_clamps_at_last_index() {
        let mut app = make_picker_app(vec!["origin", "upstream"]);
        app.current_remote_idx = 1;
        picker_navigate(&mut app, true);
        assert_eq!(app.current_remote_idx, 1);
    }

    #[test]
    fn picker_k_decreases() {
        let mut app = make_picker_app(vec!["origin", "upstream"]);
        app.current_remote_idx = 1;
        picker_navigate(&mut app, false);
        assert_eq!(app.current_remote_idx, 0);
    }

    #[test]
    fn picker_k_clamps_at_zero() {
        let mut app = make_picker_app(vec!["origin", "upstream"]);
        app.current_remote_idx = 0;
        picker_navigate(&mut app, false);
        assert_eq!(app.current_remote_idx, 0);
    }

    #[test]
    fn picker_navigate_handles_empty_list() {
        let mut app = make_picker_app(vec![]);
        picker_navigate(&mut app, true);
        assert_eq!(app.current_remote_idx, 0);
    }
}
