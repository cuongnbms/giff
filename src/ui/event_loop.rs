use crate::commit;
use crate::diff::{self, DiffSource};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{prelude::*, Terminal};
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::rebase::prepare_rebase_changes;
use super::render::ui;
use super::types::*;

/// Replicates ratatui's `List` offset rule for a fresh (default) `ListState`:
/// the offset is whatever's needed to keep the selected index visible. Used to
/// translate a click row in the file list pane back to a file index without
/// persisting `ListState` across renders.
fn file_list_offset(selected: usize, total: usize, visible_height: usize) -> usize {
    if total == 0 || visible_height == 0 || total <= visible_height {
        return 0;
    }
    let max_offset = total - visible_height;
    if selected < visible_height {
        0
    } else {
        (selected + 1 - visible_height).min(max_offset)
    }
}

/// Map the `full_file` bool to the context-lines value passed to git.
/// `None` keeps git's default 3-line context. The large value effectively
/// asks for every line; it must stay within `i32` because git parses
/// `--unified=N` into a signed int and silently overflows on larger values
/// (producing malformed hunk headers).
fn full_file_context(full_file: bool) -> Option<usize> {
    if full_file {
        Some(1_000_000_000)
    } else {
        None
    }
}

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
    if matches!(
        app.app_mode,
        AppMode::Rebase | AppMode::Log | AppMode::RemotePicker
    ) {
        return;
    }

    let context = full_file_context(app.full_file);
    let (new_changes, new_left, new_right) = match app.diff_source.fetch_with_context(context) {
        Ok(v) => v,
        Err(e) => {
            app.status_message = Some(format!("Reload failed: {}", e));
            return;
        }
    };

    if new_changes == app.file_changes && new_left == app.left_label && new_right == app.right_label
    {
        // Diff is unchanged, but operations like push/pull alter the upstream
        // ahead/behind counts shown in the header — refresh those even on the
        // no-op path so the status bar stays accurate.
        app.branch_status = diff::branch_status().ok();
        return;
    }

    let prev_selected = app.file_names.get(app.current_file_idx).cloned();

    let mut new_names: Vec<String> = new_changes.keys().cloned().collect();
    new_names.sort();

    // Drop scroll positions for files that no longer exist; keep the rest.
    app.scroll_positions
        .retain(|name, _| new_changes.contains_key(name));
    app.h_scroll_positions
        .retain(|name, _| new_changes.contains_key(name));
    for name in &new_names {
        app.scroll_positions.entry(name.clone()).or_insert(0);
        app.h_scroll_positions.entry(name.clone()).or_insert(0);
    }

    app.current_file_idx = match prev_selected {
        Some(name) => new_names.iter().position(|n| n == &name).unwrap_or(0),
        None => 0,
    };

    app.file_changes = new_changes;
    app.file_names = new_names;
    app.left_label = new_left;
    app.right_label = new_right;
    app.branch_status = diff::branch_status().ok();
    app.status_message = Some("Diff reloaded".to_string());
}

/// Replace `app`'s diff state from the given source. Resets file selection
/// and scroll positions to a clean slate. Used when switching to a commit's
/// diff or restoring the original diff source on log exit.
fn load_diff_from_source(app: &mut App, source: DiffSource) -> Result<(), String> {
    let context = full_file_context(app.full_file);
    let (changes, left, right) = source
        .fetch_with_context(context)
        .map_err(|e| e.to_string())?;
    let mut names: Vec<String> = changes.keys().cloned().collect();
    names.sort();

    app.scroll_positions.clear();
    app.h_scroll_positions.clear();
    for n in &names {
        app.scroll_positions.insert(n.clone(), 0);
        app.h_scroll_positions.insert(n.clone(), 0);
    }
    app.file_changes = changes;
    app.file_names = names;
    app.left_label = left;
    app.right_label = right;
    app.current_file_idx = 0;
    app.diff_source = source;
    Ok(())
}

/// Set a transient status message and immediately repaint so the user sees
/// progress before the next blocking git command runs on this thread.
fn flash_status<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, msg: impl Into<String>)
where
    std::io::Error: From<B::Error>,
{
    app.status_message = Some(msg.into());
    let _ = terminal.draw(|f| super::render::ui(f, app));
}

/// Generate a commit message via the claude CLI and stash it on `app` so the
/// confirmation modal can display it. No git state is changed here — staging
/// and committing happen only after the user confirms.
fn perform_commit_request<B: Backend>(terminal: &mut Terminal<B>, app: &mut App)
where
    std::io::Error: From<B::Error>,
{
    match diff::has_uncommitted_changes() {
        Ok(false) => {
            app.status_message = Some("Nothing to commit".to_string());
            return;
        }
        Err(e) => {
            app.status_message = Some(format!("Error: {}", e));
            return;
        }
        Ok(true) => {}
    }

    flash_status(terminal, app, "Generating commit message\u{2026}");

    let context = match diff::get_commit_context() {
        Ok(c) => c,
        Err(e) => {
            app.status_message = Some(format!("Error: {}", e));
            return;
        }
    };

    match commit::generate_commit_message(&context) {
        Ok(msg) => {
            app.pending_commit_message = Some(msg);
            app.show_commit_modal = true;
            app.status_message = None;
        }
        Err(e) => {
            app.status_message = Some(format!("Error: {}", e));
        }
    }
}

/// User confirmed the pending commit message: stage everything and commit.
fn perform_commit_confirm<B: Backend>(terminal: &mut Terminal<B>, app: &mut App)
where
    std::io::Error: From<B::Error>,
{
    let message = match app.pending_commit_message.take() {
        Some(m) => m,
        None => {
            app.show_commit_modal = false;
            return;
        }
    };

    app.show_commit_modal = false;
    flash_status(terminal, app, "Committing\u{2026}");

    if let Err(e) = diff::stage_all() {
        app.status_message = Some(format!("Stage failed: {}", e));
        return;
    }
    if let Err(e) = diff::commit_with_message(&message) {
        app.status_message = Some(format!("Commit failed: {}", e));
        return;
    }

    let subject = message.lines().next().unwrap_or("").to_string();
    app.status_message = Some(format!("Committed: {}", subject));
    app.branch_status = diff::branch_status().ok();
    reload_diff(app);
}

fn cancel_commit(app: &mut App) {
    app.pending_commit_message = None;
    app.show_commit_modal = false;
    app.status_message = Some("Commit cancelled".to_string());
}

fn perform_sync<B: Backend>(terminal: &mut Terminal<B>, app: &mut App)
where
    std::io::Error: From<B::Error>,
{
    flash_status(terminal, app, "Syncing\u{2026}");

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
        Ok(Some(upstream)) => sync_with_upstream(terminal, app, &upstream),
        Ok(None) => sync_without_upstream(terminal, app),
        Err(e) => {
            app.status_message = Some(format!("Sync failed: {}", e));
        }
    }
}

fn sync_with_upstream<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, upstream: &str)
where
    std::io::Error: From<B::Error>,
{
    flash_status(terminal, app, format!("Pulling from {}\u{2026}", upstream));
    if let Err(e) = diff::pull_rebase() {
        app.status_message = Some(format!("Pull failed: {}", e));
        return;
    }
    flash_status(terminal, app, format!("Pushing to {}\u{2026}", upstream));
    if let Err(e) = diff::push() {
        app.status_message = Some(format!("Push failed: {}", e));
        return;
    }
    app.status_message = Some(format!("Synced with {}", upstream));
    reload_diff(app);
}

fn sync_without_upstream<B: Backend>(terminal: &mut Terminal<B>, app: &mut App)
where
    std::io::Error: From<B::Error>,
{
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
            push_to_remote(terminal, app, &remote, &branch);
        }
        PushDecision::NeedsPicker(list) => {
            app.remotes = list;
            app.current_remote_idx = 0;
            app.app_mode = AppMode::RemotePicker;
        }
    }
}

fn push_to_remote<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, remote: &str, branch: &str)
where
    std::io::Error: From<B::Error>,
{
    flash_status(
        terminal,
        app,
        format!("Pushing {} \u{2192} {}/{}\u{2026}", branch, remote, branch),
    );
    match diff::push_set_upstream(remote, branch) {
        Ok(()) => {
            app.status_message = Some(format!("Pushed {} \u{2192} {}/{}", branch, remote, branch));
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

fn picker_confirm<B: Backend>(terminal: &mut Terminal<B>, app: &mut App)
where
    std::io::Error: From<B::Error>,
{
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
    push_to_remote(terminal, app, &remote, &branch);
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

                    // Handle commit confirmation modal if shown
                    if app.show_commit_modal {
                        match key.code {
                            KeyCode::Enter | KeyCode::Char('y') => {
                                perform_commit_confirm(terminal, &mut app);
                            }
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => {
                                cancel_commit(&mut app);
                            }
                            _ => {}
                        }
                        continue;
                    }

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

                    // Shift+Left / Shift+Right scroll the diff pane horizontally
                    // so long lines that overflow the pane can be inspected.
                    if matches!(app.app_mode, AppMode::Diff)
                        && key.modifiers.contains(KeyModifiers::SHIFT)
                        && matches!(key.code, KeyCode::Left | KeyCode::Right)
                    {
                        const H_STEP: usize = 5;
                        if let Some(file) = app.file_names.get(app.current_file_idx) {
                            let cur = *app.h_scroll_positions.get(file).unwrap_or(&0);
                            let next = if matches!(key.code, KeyCode::Right) {
                                cur.saturating_add(H_STEP)
                            } else {
                                cur.saturating_sub(H_STEP)
                            };
                            app.h_scroll_positions.insert(file.clone(), next);
                        }
                        continue;
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
                            AppMode::RemotePicker => picker_confirm(terminal, &mut app),
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
                                perform_sync(terminal, &mut app);
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
                        KeyCode::Char('c') => match app.app_mode {
                            AppMode::Rebase => commit_rebase_changes(&mut app),
                            AppMode::Diff => perform_commit_request(terminal, &mut app),
                            _ => {}
                        },
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
                                    app.current_commit_idx =
                                        (app.current_commit_idx + page).min(app.commits.len() - 1);
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
                                app.current_commit_idx = app.commits.len().saturating_sub(1);
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
                        KeyCode::Char('t') if !app.theme_cycle.is_empty() => {
                            // Cycle through available themes
                            app.theme_cycle_idx = (app.theme_cycle_idx + 1) % app.theme_cycle.len();
                            app.theme = app.theme_cycle[app.theme_cycle_idx].clone();
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
                        KeyCode::Char('w') => {
                            if let AppMode::Diff = app.app_mode {
                                app.wrap_mode = !app.wrap_mode;
                                if app.wrap_mode {
                                    for v in app.h_scroll_positions.values_mut() {
                                        *v = 0;
                                    }
                                }
                                app.status_message = Some(
                                    if app.wrap_mode {
                                        "Wrap: ON"
                                    } else {
                                        "Wrap: OFF"
                                    }
                                    .to_string(),
                                );
                            }
                        }
                        KeyCode::Char('f') => {
                            if let AppMode::Diff = app.app_mode {
                                app.full_file = !app.full_file;
                                let context = full_file_context(app.full_file);
                                match app.diff_source.fetch_with_context(context) {
                                    Ok((changes, left, right)) => {
                                        let mut names: Vec<String> =
                                            changes.keys().cloned().collect();
                                        names.sort();

                                        app.scroll_positions
                                            .retain(|name, _| changes.contains_key(name));
                                        app.h_scroll_positions
                                            .retain(|name, _| changes.contains_key(name));
                                        for name in &names {
                                            app.scroll_positions.entry(name.clone()).or_insert(0);
                                            app.h_scroll_positions.entry(name.clone()).or_insert(0);
                                        }

                                        let prev =
                                            app.file_names.get(app.current_file_idx).cloned();
                                        app.current_file_idx = prev
                                            .and_then(|cur| names.iter().position(|n| n == &cur))
                                            .unwrap_or(0);

                                        app.file_changes = changes;
                                        app.file_names = names;
                                        app.left_label = left;
                                        app.right_label = right;
                                        app.status_message = Some(
                                            if app.full_file {
                                                "Full file: ON"
                                            } else {
                                                "Full file: OFF"
                                            }
                                            .to_string(),
                                        );
                                    }
                                    Err(e) => {
                                        // Revert toggle on failure so state stays consistent.
                                        app.full_file = !app.full_file;
                                        app.status_message =
                                            Some(format!("Full-file toggle failed: {}", e));
                                    }
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
                    if app.show_help_modal || app.show_rebase_modal || app.show_commit_modal {
                        continue;
                    }
                    let size = terminal.size()?;
                    let scroll_amount: usize = 3;
                    let file_list_width =
                        (size.width as u32 * app.file_list_width_pct as u32 / 100) as u16;
                    if matches!(app.app_mode, AppMode::Diff) {
                        let on_divider = mouse.row > 0
                            && mouse.row < size.height.saturating_sub(1)
                            && (mouse.column as i32 - file_list_width as i32).abs() <= 1;
                        let in_file_list = mouse.column < file_list_width
                            && mouse.row > 0
                            && mouse.row < size.height.saturating_sub(1);
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) if on_divider => {
                                app.resizing_divider = true;
                                continue;
                            }
                            MouseEventKind::Drag(MouseButton::Left)
                                if app.resizing_divider && size.width > 0 =>
                            {
                                let raw = mouse.column as u32 * 100 / size.width as u32;
                                app.file_list_width_pct = (raw as u16).clamp(10, 60);
                                continue;
                            }
                            MouseEventKind::Up(MouseButton::Left) if app.resizing_divider => {
                                app.resizing_divider = false;
                                continue;
                            }
                            MouseEventKind::Down(MouseButton::Left)
                                if in_file_list && !app.file_names.is_empty() =>
                            {
                                // File list inner rows start at y=2 (header at 0,
                                // top border at 1). Inner height = total - header
                                // - help - top/bottom borders = size.height - 4.
                                const INNER_TOP: u16 = 2;
                                if mouse.row >= INNER_TOP {
                                    let relative = (mouse.row - INNER_TOP) as usize;
                                    let visible_height = size.height.saturating_sub(4) as usize;
                                    let offset = file_list_offset(
                                        app.current_file_idx,
                                        app.file_names.len(),
                                        visible_height,
                                    );
                                    let target = offset + relative;
                                    if target < app.file_names.len() {
                                        app.current_file_idx = target;
                                        app.focused_pane = Pane::FileList;
                                    }
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match mouse.kind {
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            if mouse.row == 0 || mouse.row >= size.height.saturating_sub(1) {
                                continue;
                            }
                            let is_down = matches!(mouse.kind, MouseEventKind::ScrollDown);
                            match app.app_mode {
                                AppMode::Diff => {
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
                                            app.current_commit_idx = (app.current_commit_idx
                                                + scroll_amount)
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
                        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                            if mouse.row == 0 || mouse.row >= size.height.saturating_sub(1) {
                                continue;
                            }
                            if !matches!(app.app_mode, AppMode::Diff) {
                                continue;
                            }
                            if mouse.column < file_list_width {
                                continue;
                            }
                            if let Some(file) = app.file_names.get(app.current_file_idx) {
                                let cur = *app.h_scroll_positions.get(file).unwrap_or(&0);
                                let next = if matches!(mouse.kind, MouseEventKind::ScrollRight) {
                                    cur.saturating_add(scroll_amount)
                                } else {
                                    cur.saturating_sub(scroll_amount)
                                };
                                app.h_scroll_positions.insert(file.clone(), next);
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
            h_scroll_positions: HashMap::new(),
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
            branch_status: None,
            file_list_width_pct: 20,
            resizing_divider: false,
            full_file: false,
            wrap_mode: false,
            pending_commit_message: None,
            show_commit_modal: false,
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

    #[test]
    fn full_file_context_off_returns_none() {
        assert_eq!(full_file_context(false), None);
    }

    #[test]
    fn full_file_context_on_returns_huge_but_in_range() {
        let ctx = full_file_context(true).expect("full_file=true should yield context");
        // Must stay within i32 (git overflows --unified=N parsed as signed int).
        assert!(ctx <= i32::MAX as usize);
        // Must be large enough to cover any realistic file.
        assert!(ctx >= 1_000_000);
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

    // ── file_list_offset ──────────────────────────────────────────────

    #[test]
    fn file_list_offset_empty_or_zero_height() {
        assert_eq!(file_list_offset(0, 0, 10), 0);
        assert_eq!(file_list_offset(0, 5, 0), 0);
    }

    #[test]
    fn file_list_offset_list_fits_in_view() {
        // 5 items, height 10 → no scrolling needed.
        assert_eq!(file_list_offset(0, 5, 10), 0);
        assert_eq!(file_list_offset(4, 5, 10), 0);
    }

    #[test]
    fn file_list_offset_selected_in_first_page() {
        // Selected within the first `height` items → offset stays 0.
        assert_eq!(file_list_offset(0, 100, 10), 0);
        assert_eq!(file_list_offset(9, 100, 10), 0);
    }

    #[test]
    fn file_list_offset_selected_past_first_page() {
        // Selected = 10, height 10 → offset = 1 so selected is on the last row.
        assert_eq!(file_list_offset(10, 100, 10), 1);
        assert_eq!(file_list_offset(50, 100, 10), 41);
    }

    #[test]
    fn file_list_offset_clamps_to_max() {
        // Near the end, offset clamps so the last `height` items show.
        assert_eq!(file_list_offset(99, 100, 10), 90);
    }
}
