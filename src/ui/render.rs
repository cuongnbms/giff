use ratatui::{
    prelude::*,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use std::cell::RefCell;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::diff::{FileMeta, FileStatus, LineChange};

use super::event_loop::{build_file_tree, TreeRow};
use super::rebase::render_rebase_ui;
use super::syntax::HighlightCache;
use super::theme::Theme;
use super::types::*;

pub fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    // Apply the theme's root background to the entire frame
    let bg = Block::default().style(Style::default().bg(app.theme.bg_default));
    f.render_widget(bg, size);

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header
            Constraint::Min(0),    // Content
            Constraint::Length(1), // Help
        ])
        .split(size);

    render_header(f, app, main_chunks[0]);

    // Clamp scroll position so it cannot exceed content bounds
    if matches!(app.app_mode, AppMode::Diff) {
        clamp_scroll(app, main_chunks[1].width, main_chunks[1].height);
        if !app.wrap_mode {
            clamp_h_scroll(app, main_chunks[1].width);
        }
        follow_cursor(app, main_chunks[1].height);
    }

    match app.app_mode {
        AppMode::Diff => match app.view_mode {
            ViewMode::SideBySide => {
                let pct = app.file_list_width_pct;
                let rest = 100u16.saturating_sub(pct);
                let left = rest / 2;
                let right = rest - left;
                let content_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(pct),
                        Constraint::Percentage(left),
                        Constraint::Percentage(right),
                    ])
                    .split(main_chunks[1]);

                render_file_list(f, app, content_chunks[0]);
                if !app.file_names.is_empty() {
                    render_side_by_side(f, app, content_chunks[1], content_chunks[2]);
                }
            }
            ViewMode::Unified => {
                let pct = app.file_list_width_pct;
                let content_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(pct),
                        Constraint::Percentage(100u16.saturating_sub(pct)),
                    ])
                    .split(main_chunks[1]);

                render_file_list(f, app, content_chunks[0]);
                if !app.file_names.is_empty() {
                    render_unified_diff(f, app, content_chunks[1]);
                }
            }
        },
        AppMode::Rebase => {
            render_rebase_ui(f, app, main_chunks[1]);
        }
        AppMode::Log => {
            render_log_ui(f, app, main_chunks[1]);
        }
        AppMode::RemotePicker => {}
    }

    render_help(f, app, main_chunks[2]);

    if app.show_rebase_modal {
        render_rebase_notification(f, app, size);
    }

    if app.show_help_modal {
        render_help_modal(f, app, size);
    }

    if matches!(app.app_mode, AppMode::RemotePicker) {
        render_remote_picker(f, app, size);
    }

    if app.show_commit_modal {
        render_commit_modal(f, app, size);
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let view_mode_base = match app.view_mode {
        ViewMode::SideBySide => "Side-by-Side",
        ViewMode::Unified => "Unified",
    };
    let view_mode = if app.wrap_mode {
        format!("{} + Wrap", view_mode_base)
    } else {
        view_mode_base.to_string()
    };
    let mode = match app.app_mode {
        AppMode::Diff => {
            if app.log_return_source.is_some() {
                "DIFF (commit)"
            } else {
                "DIFF"
            }
        }
        AppMode::Rebase => "REBASE",
        AppMode::Log => "LOG",
        AppMode::RemotePicker => "SYNC",
    };
    let file_count = app.file_names.len();
    let current = if file_count > 0 {
        app.current_file_idx + 1
    } else {
        0
    };
    let current_file = app
        .file_names
        .get(app.current_file_idx)
        .map(|s| s.as_str())
        .unwrap_or("");

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));
    if let Some(bs) = &app.branch_status {
        if !bs.name.is_empty() {
            spans.extend(branch_status_spans(t, bs));
            spans.push(Span::styled(
                " \u{2502} ",
                Style::default().fg(t.border_dim),
            ));
        }
    }
    spans.push(Span::styled(
        format!("{} \u{2192} {}", app.left_label, app.right_label),
        Style::default().fg(t.fg_normal),
    ));
    spans.push(Span::styled(
        " \u{2502} ",
        Style::default().fg(t.border_dim),
    ));
    spans.push(Span::styled(mode.to_owned(), Style::default().fg(t.accent)));
    spans.push(Span::styled(
        " \u{2502} ",
        Style::default().fg(t.border_dim),
    ));
    spans.push(Span::styled(view_mode, Style::default().fg(t.fg_dim)));

    if !current_file.is_empty() {
        spans.push(Span::styled(
            " \u{2502} ",
            Style::default().fg(t.border_dim),
        ));
        spans.push(Span::styled(
            current_file.to_owned(),
            Style::default().fg(t.fg_bright),
        ));
    }

    spans.push(Span::styled(
        " \u{2502} ",
        Style::default().fg(t.border_dim),
    ));
    spans.push(Span::styled(
        format!("{}/{}", current, file_count),
        Style::default().fg(t.fg_dim),
    ));

    let header = Paragraph::new(Line::from(spans)).style(Style::default().bg(t.bg_header));
    f.render_widget(header, area);
}

fn branch_status_spans<'a>(t: &Theme, bs: &crate::diff::BranchStatus) -> Vec<Span<'a>> {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled("\u{e0a0} ", Style::default().fg(t.accent)));
    spans.push(Span::styled(
        bs.name.clone(),
        Style::default()
            .fg(t.fg_bright)
            .add_modifier(Modifier::BOLD),
    ));
    if bs.upstream.is_some() {
        spans.push(Span::styled(
            format!(" \u{2191}{}", bs.ahead),
            Style::default().fg(if bs.ahead > 0 { t.fg_added } else { t.fg_dim }),
        ));
        spans.push(Span::styled(
            format!(" \u{2193}{}", bs.behind),
            Style::default().fg(if bs.behind > 0 {
                t.fg_removed
            } else {
                t.fg_dim
            }),
        ));
    }
    spans
}

/// Pick the file-list badge for a file: red `D` for deletes, green `A` for
/// new files, dim `R`/`r` for pure/partial renames, else none. Returns the
/// badge text (including its leading space) and the style to draw it with.
fn status_badge(meta: Option<&FileMeta>, t: &Theme) -> Option<(&'static str, Style)> {
    let meta = meta?;
    match meta.status {
        FileStatus::Deleted => Some((" D", Style::default().fg(t.fg_removed))),
        FileStatus::Added => Some((" A", Style::default().fg(t.fg_added))),
        FileStatus::Modified => {
            if meta.is_pure_rename() {
                Some((" R", Style::default().fg(t.fg_dim)))
            } else if meta.is_rename() {
                Some((" r", Style::default().fg(t.fg_dim)))
            } else {
                None
            }
        }
    }
}

pub fn render_file_list(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let is_focused = matches!(app.focused_pane, Pane::FileList);
    let border_color = if is_focused {
        t.border_focused
    } else {
        t.border_dim
    };
    let title_style = if is_focused {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.fg_dim)
    };

    let block = Block::default()
        .title(Span::styled(" Files ", title_style))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));

    if app.file_names.is_empty() {
        let empty = Paragraph::new(Span::styled("  No changes", Style::default().fg(t.fg_dim)))
            .block(block);
        f.render_widget(empty, area);
        return;
    }

    // Borders (2) + highlight symbol "▌ " (2)
    const FILE_LIST_CHROME_WIDTH: u16 = 4;
    let inner_width = area.width.saturating_sub(FILE_LIST_CHROME_WIDTH) as usize;

    if app.file_tree_view {
        render_file_tree(f, app, area, block, inner_width);
        return;
    }

    let items: Vec<ListItem> = app
        .file_names
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let (adds, dels) = count_file_changes(app, file);
            let is_current = i == app.current_file_idx;
            let name_style = if is_current {
                Style::default()
                    .fg(t.fg_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg_normal)
            };

            // Reserve room for a status badge (` D`/` A`/` R`) so the file
            // name and stats reflow correctly when a badge is present.
            let badge = status_badge(app.file_meta.get(file), t);
            let badge_width = badge.map(|(s, _)| s.len()).unwrap_or(0);

            let (stat_spans, stats_width) = build_file_stats(adds, dels, t);
            let max_name_width = inner_width
                .saturating_sub(stats_width)
                .saturating_sub(badge_width);
            let (file_part, dir_part) = split_path_for_display(file);
            let (file_disp, dir_disp) = fit_file_and_dir(&file_part, &dir_part, max_name_width);

            let mut spans = vec![Span::styled(file_disp, name_style)];
            if !dir_disp.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", dir_disp),
                    Style::default().fg(t.fg_dim),
                ));
            }
            if let Some((text, style)) = badge {
                spans.push(Span::styled(text.to_string(), style));
            }
            spans.extend(stat_spans);

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(t.bg_selection))
        .highlight_symbol("\u{258c} ");

    f.render_stateful_widget(
        list,
        area,
        &mut ratatui::widgets::ListState::default().with_selected(Some(app.current_file_idx)),
    );
}

/// Render the file panel as an indented directory tree. Directory rows are
/// dim, non-interactive labels; file rows show the basename plus the same
/// stats/rename badges as the flat list. Selection (`current_file_idx`, an
/// index into `file_names`) is mapped to the matching file row for highlight
/// and auto-scroll.
fn render_file_tree(f: &mut Frame, app: &App, area: Rect, block: Block<'_>, inner_width: usize) {
    let t = &app.theme;
    let rows = build_file_tree(&app.file_names);

    // Map current_file_idx -> position of its File row in `rows`.
    let mut selected_row: Option<usize> = None;
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(row_i, row)| match row {
            TreeRow::Dir { label, depth } => {
                let indent = "  ".repeat(*depth);
                let max_label_width = inner_width.saturating_sub(indent.len());
                let label_disp = truncate_tail(&format!("{}/", label), max_label_width);
                ListItem::new(Line::from(Span::styled(
                    format!("{}{}", indent, label_disp),
                    Style::default().fg(t.fg_dim),
                )))
            }
            TreeRow::File { file_idx, depth } => {
                let file = &app.file_names[*file_idx];
                let is_current = *file_idx == app.current_file_idx;
                if is_current {
                    selected_row = Some(row_i);
                }
                let name_style = if is_current {
                    Style::default()
                        .fg(t.fg_bright)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.fg_normal)
                };

                // Reserve room for a status badge (` D`/` A`/` R`) so the file
                // name and stats reflow correctly when a badge is present.
                let badge = status_badge(app.file_meta.get(file), t);
                let badge_width = badge.map(|(s, _)| s.len()).unwrap_or(0);

                let (adds, dels) = count_file_changes(app, file);
                let (stat_spans, stats_width) = build_file_stats(adds, dels, t);

                let indent = "  ".repeat(*depth);
                let indent_width = indent.len();
                let max_name_width = inner_width
                    .saturating_sub(stats_width)
                    .saturating_sub(badge_width)
                    .saturating_sub(indent_width);

                let (file_part, _) = split_path_for_display(file);
                let (file_disp, _) = fit_file_and_dir(&file_part, "", max_name_width);

                let mut spans = vec![Span::styled(format!("{}{}", indent, file_disp), name_style)];
                if let Some((text, style)) = badge {
                    spans.push(Span::styled(text.to_string(), style));
                }
                spans.extend(stat_spans);
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(t.bg_selection))
        .highlight_symbol("\u{258c} ");

    f.render_stateful_widget(
        list,
        area,
        &mut ratatui::widgets::ListState::default().with_selected(selected_row),
    );
}

/// Whether logical line `abs` is part of the selection or under the cursor.
fn row_is_selected(abs: usize, cursor: Option<usize>, selection: Option<(usize, usize)>) -> bool {
    if let Some((lo, hi)) = selection {
        if abs >= lo && abs <= hi {
            return true;
        }
    }
    cursor == Some(abs)
}

/// Paint `bg` onto every span of the gutter+content rows that fall in the
/// selection/cursor. `start` is the absolute logical index of the first row.
fn highlight_selected_rows(
    gutter: &mut [Line<'static>],
    content: &mut [Line<'static>],
    start: usize,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    bg: ratatui::style::Color,
) {
    for (i, (g, c)) in gutter.iter_mut().zip(content.iter_mut()).enumerate() {
        if row_is_selected(start + i, cursor, selection) {
            for span in g.spans.iter_mut().chain(c.spans.iter_mut()) {
                span.style = span.style.bg(bg);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_diff_pane(
    f: &mut Frame,
    title: &str,
    lines: &[(usize, String)],
    filename: &str,
    scroll: usize,
    h_scroll: usize,
    wrap_mode: bool,
    is_focused: bool,
    area: Rect,
    theme: &Theme,
    cache: &RefCell<HighlightCache>,
    source: Option<&[String]>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) {
    let border_color = if is_focused {
        theme.border_focused
    } else {
        theme.border_dim
    };
    let title_style = if is_focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg_dim)
    };

    let block = Block::default()
        // Placeholder title; populated after we know total visible row count.
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    let visible_height = inner.height as usize;

    let (total_rows, title_text) = if wrap_mode {
        let rows = total_wrapped_rows(lines, inner.width as usize);
        let title_text = if rows > visible_height {
            let max_scroll = rows.saturating_sub(visible_height);
            let pos = scroll.min(max_scroll);
            let pct = (pos * 100).checked_div(max_scroll).unwrap_or(0);
            format!(" {} ({}%) \u{2502} wrap ", title, pct)
        } else {
            format!(" {} \u{2502} wrap ", title)
        };
        (rows, title_text)
    } else {
        // Highlighting produces exactly one rendered row per input line.
        let rows = lines.len();
        let title_text = if rows > visible_height {
            let max_scroll = rows.saturating_sub(visible_height);
            let pos = scroll.min(max_scroll);
            let pct = (pos * 100).checked_div(max_scroll).unwrap_or(0);
            if h_scroll > 0 {
                format!(" {} ({}%) \u{2192}{} ", title, pct, h_scroll)
            } else {
                format!(" {} ({}%) ", title, pct)
            }
        } else if h_scroll > 0 {
            format!(" {} \u{2192}{} ", title, h_scroll)
        } else {
            format!(" {} ", title)
        };
        (rows, title_text)
    };

    let block = block.title(Span::styled(title_text, title_style));
    f.render_widget(block, area);

    if wrap_mode {
        // Wrap mode scrolls in visual rows and lets ratatui do the wrapping, so
        // we can't slice an arbitrary window by logical line. But we don't need
        // the whole file either: feed ratatui only the logical lines up to the
        // bottom of the viewport (highlighted incrementally) and let it wrap +
        // scroll as usual. This keeps a file switch O(visible) instead of
        // highlighting every line. Walk visual-row counts (cheap, no syntax
        // work) to find that last line.
        let pane_inner = inner.width as usize;
        let needed_rows = scroll.saturating_add(visible_height);
        let mut acc = 0usize;
        let mut end = 0usize;
        while end < lines.len() && acc < needed_rows {
            acc += merged_line_rows(&lines[end], pane_inner);
            end += 1;
        }
        // Overscan a couple of lines: ratatui word-wraps while `merged_line_rows`
        // estimates by width, so the counts can differ by a row.
        let end = end.saturating_add(2).min(lines.len());

        // Merge the gutter into each content line so wrap keeps line numbers on
        // the first visual row; continuation rows have no gutter.
        let (gutter_lines, content_lines) = cache
            .borrow_mut()
            .window_src(lines, filename, theme, 0, end, source);
        // ratatui Paragraph::scroll() accepts (u16, u16); clamp for >65k rows.
        let scroll_u16 = scroll.min(u16::MAX as usize) as u16;
        let mut merged: Vec<Line<'static>> = gutter_lines
            .into_iter()
            .zip(content_lines)
            .map(|(g, c)| {
                let mut spans = g.spans;
                spans.extend(c.spans);
                Line::from(spans)
            })
            .collect();

        // Wrap window starts at logical line 0, so the absolute index is the
        // position in `merged`.
        for (i, line) in merged.iter_mut().enumerate() {
            if row_is_selected(i, cursor, selection) {
                for span in line.spans.iter_mut() {
                    span.style = span.style.bg(theme.bg_selection);
                }
            }
        }

        if inner.width > 0 && inner.height > 0 {
            let paragraph = Paragraph::new(Text::from(merged))
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll_u16, 0));
            f.render_widget(paragraph, inner);
        }
    } else {
        // Non-wrap: copy only the visible window of highlighted rows so a
        // scroll keypress is O(visible) instead of O(file). The slice already
        // starts at `scroll`, so the paragraphs render at vertical offset 0.
        let (mut gutter_lines, mut content_lines) =
            cache
                .borrow_mut()
                .window_src(lines, filename, theme, scroll, visible_height, source);

        highlight_selected_rows(
            &mut gutter_lines,
            &mut content_lines,
            scroll,
            cursor,
            selection,
            theme.bg_selection,
        );

        // Pin the line-number gutter + change marker (7 cols) so they stay
        // visible when the user scrolls the code horizontally.
        const GUTTER_WIDTH: u16 = 7;
        let gutter_width = GUTTER_WIDTH.min(inner.width);
        let gutter_area = Rect {
            x: inner.x,
            y: inner.y,
            width: gutter_width,
            height: inner.height,
        };
        let content_area = Rect {
            x: inner.x + gutter_width,
            y: inner.y,
            width: inner.width.saturating_sub(gutter_width),
            height: inner.height,
        };

        let h_scroll_u16 = h_scroll.min(u16::MAX as usize) as u16;

        let gutter_paragraph = Paragraph::new(Text::from(gutter_lines));
        f.render_widget(gutter_paragraph, gutter_area);

        if content_area.width > 0 {
            let content_paragraph =
                Paragraph::new(Text::from(content_lines)).scroll((0, h_scroll_u16));
            f.render_widget(content_paragraph, content_area);
        }
    }

    // Scrollbar
    if total_rows > visible_height {
        let scrollbar_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let max_scroll = total_rows.saturating_sub(visible_height);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll.min(max_scroll));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }
}

/// Number of visual rows a line of `line_width` columns occupies when wrapped
/// to `available_width`. Empty lines still occupy one row.
fn wrap_rows(line_width: usize, available_width: usize) -> usize {
    if available_width == 0 {
        return 1;
    }
    if line_width == 0 {
        return 1;
    }
    line_width.div_ceil(available_width)
}

/// Visual rows occupied by a single LineChange when rendered with the
/// merged-gutter wrap layout into `pane_inner_width` columns. Gap rows
/// (line_num == 0) collapse to one empty row.
pub(super) fn merged_line_rows(line: &LineChange, pane_inner_width: usize) -> usize {
    const GUTTER_WIDTH: usize = 7;
    if line.0 == 0 {
        return 1;
    }
    let content = line
        .1
        .strip_prefix('+')
        .or_else(|| line.1.strip_prefix('-'))
        .or_else(|| line.1.strip_prefix(' '))
        .unwrap_or(&line.1);
    let w = GUTTER_WIDTH + UnicodeWidthStr::width(content);
    wrap_rows(w, pane_inner_width)
}

/// Sum of visual rows across `lines` using the merged-gutter wrap layout.
fn total_wrapped_rows(lines: &[LineChange], pane_inner_width: usize) -> usize {
    lines
        .iter()
        .map(|l| merged_line_rows(l, pane_inner_width))
        .sum()
}

/// Allocation-free counterpart to `align_lines` + `total_wrapped_rows`:
/// returns the number of visual rows used by the aligned side-by-side view
/// after pair-wise wrap-padding (each pair takes max of the two sides' rows).
pub(super) fn aligned_wrapped_row_count(
    base_lines: &[LineChange],
    head_lines: &[LineChange],
    pane_inner_width: usize,
) -> usize {
    let mut total = 0usize;
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            let b_start = bi;
            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                bi += 1;
            }
            let h_start = hi;
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                hi += 1;
            }
            let b_len = bi - b_start;
            let h_len = hi - h_start;
            let max_len = b_len.max(h_len);
            for i in 0..max_len {
                let b_rows = if i < b_len {
                    merged_line_rows(&base_lines[b_start + i], pane_inner_width)
                } else {
                    1
                };
                let h_rows = if i < h_len {
                    merged_line_rows(&head_lines[h_start + i], pane_inner_width)
                } else {
                    1
                };
                total += b_rows.max(h_rows);
            }
        } else if bi < base_lines.len() && hi < head_lines.len() {
            let b_rows = merged_line_rows(&base_lines[bi], pane_inner_width);
            let h_rows = merged_line_rows(&head_lines[hi], pane_inner_width);
            total += b_rows.max(h_rows);
            bi += 1;
            hi += 1;
        } else if bi < base_lines.len() {
            total += merged_line_rows(&base_lines[bi], pane_inner_width);
            bi += 1;
        } else {
            total += merged_line_rows(&head_lines[hi], pane_inner_width);
            hi += 1;
        }
    }
    total
}

/// Allocation-free counterpart to `build_unified_lines` + `total_wrapped_rows`.
pub(super) fn unified_wrapped_row_count(
    base_lines: &[LineChange],
    head_lines: &[LineChange],
    pane_inner_width: usize,
) -> usize {
    let mut total = 0usize;
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                total += merged_line_rows(&base_lines[bi], pane_inner_width);
                bi += 1;
            }
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                total += merged_line_rows(&head_lines[hi], pane_inner_width);
                hi += 1;
            }
        } else if bi < base_lines.len() {
            total += merged_line_rows(&base_lines[bi], pane_inner_width);
            bi += 1;
            if hi < head_lines.len() {
                hi += 1;
            }
        } else {
            total += merged_line_rows(&head_lines[hi], pane_inner_width);
            hi += 1;
        }
    }
    total
}

/// In wrap mode, pad each (base, head) pair from `align_lines` so both sides
/// have equal visual row counts. Inserts blank `(0, "")` entries on the
/// shorter side after each logical line.
pub(super) fn pad_aligned_for_wrap(
    aligned_base: Vec<LineChange>,
    aligned_head: Vec<LineChange>,
    pane_inner_width: usize,
) -> (Vec<LineChange>, Vec<LineChange>) {
    debug_assert_eq!(aligned_base.len(), aligned_head.len());
    let mut out_base = Vec::with_capacity(aligned_base.len());
    let mut out_head = Vec::with_capacity(aligned_head.len());
    for (b, h) in aligned_base.into_iter().zip(aligned_head) {
        let b_rows = merged_line_rows(&b, pane_inner_width);
        let h_rows = merged_line_rows(&h, pane_inner_width);
        let max_rows = b_rows.max(h_rows);
        out_base.push(b);
        out_head.push(h);
        for _ in b_rows..max_rows {
            out_base.push((0, String::new()));
        }
        for _ in h_rows..max_rows {
            out_head.push((0, String::new()));
        }
    }
    (out_base, out_head)
}

/// Produce aligned line vectors for side-by-side display.
/// Gap lines are represented as `(0, String::new())`.
pub(super) fn align_lines(
    base_lines: &[LineChange],
    head_lines: &[LineChange],
) -> (Vec<LineChange>, Vec<LineChange>) {
    let mut aligned_base = Vec::new();
    let mut aligned_head = Vec::new();
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            // Collect consecutive change lines from each side
            let mut b_chunk = Vec::new();
            let mut h_chunk = Vec::new();

            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                b_chunk.push(base_lines[bi].clone());
                bi += 1;
            }
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                h_chunk.push(head_lines[hi].clone());
                hi += 1;
            }

            // Pair change lines, padding the shorter side with gaps
            let max_len = b_chunk.len().max(h_chunk.len());
            for i in 0..max_len {
                aligned_base.push(b_chunk.get(i).cloned().unwrap_or((0, String::new())));
                aligned_head.push(h_chunk.get(i).cloned().unwrap_or((0, String::new())));
            }
        } else if bi < base_lines.len() && hi < head_lines.len() {
            // Both are context lines
            aligned_base.push(base_lines[bi].clone());
            aligned_head.push(head_lines[hi].clone());
            bi += 1;
            hi += 1;
        } else if bi < base_lines.len() {
            aligned_base.push(base_lines[bi].clone());
            aligned_head.push((0, String::new()));
            bi += 1;
        } else {
            aligned_base.push((0, String::new()));
            aligned_head.push(head_lines[hi].clone());
            hi += 1;
        }
    }

    (aligned_base, aligned_head)
}

/// Compute the number of aligned lines without allocating full vectors.
pub(super) fn aligned_line_count(base_lines: &[LineChange], head_lines: &[LineChange]) -> usize {
    let mut count = 0;
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            let mut b_count = 0;
            let mut h_count = 0;
            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                b_count += 1;
                bi += 1;
            }
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                h_count += 1;
                hi += 1;
            }
            count += b_count.max(h_count);
        } else {
            if bi < base_lines.len() {
                bi += 1;
            }
            if hi < head_lines.len() {
                hi += 1;
            }
            count += 1;
        }
    }

    count
}

/// Compute the number of unified diff lines without allocating.
pub(super) fn unified_line_count(base_lines: &[LineChange], head_lines: &[LineChange]) -> usize {
    let mut count = 0;
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                count += 1;
                bi += 1;
            }
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                count += 1;
                hi += 1;
            }
        } else {
            if bi < base_lines.len() {
                bi += 1;
            }
            if hi < head_lines.len() {
                hi += 1;
            }
            count += 1;
        }
    }

    count
}

fn render_side_by_side(f: &mut Frame, app: &App, base_area: Rect, head_area: Rect) {
    let current_file = match app.file_names.get(app.current_file_idx) {
        Some(f) => f,
        None => return,
    };
    let (base_lines, head_lines) = match app.file_changes.get(current_file) {
        Some(c) => c,
        None => return,
    };
    let scroll = *app.scroll_positions.get(current_file).unwrap_or(&0);
    let h_scroll = *app.h_scroll_positions.get(current_file).unwrap_or(&0);
    let is_focused = matches!(app.focused_pane, Pane::DiffContent);

    // Full base/head text for priming the highlighter's parse state, so
    // multi-line constructs opened above the visible hunk are colored right.
    let (base_src, head_src) = match app.full_content.get(current_file) {
        Some((b, h)) => (Some(b.as_slice()), Some(h.as_slice())),
        None => (None, None),
    };

    let (aligned_base, aligned_head) = align_lines(base_lines, head_lines);
    // In wrap mode, pad each pair so both panes' visual rows stay aligned.
    // Use the smaller pane width for wrap math so both sides agree on counts.
    let (aligned_base, aligned_head) = if app.wrap_mode {
        let pane_inner = base_area.width.min(head_area.width).saturating_sub(2) as usize;
        pad_aligned_for_wrap(aligned_base, aligned_head, pane_inner)
    } else {
        (aligned_base, aligned_head)
    };

    render_diff_pane(
        f,
        &app.left_label,
        &aligned_base,
        current_file,
        scroll,
        h_scroll,
        app.wrap_mode,
        is_focused,
        base_area,
        &app.theme,
        &app.highlight_cache,
        base_src,
        if is_focused {
            Some(app.diff_cursor)
        } else {
            None
        },
        app.selection_range(),
    );
    render_diff_pane(
        f,
        &app.right_label,
        &aligned_head,
        current_file,
        scroll,
        h_scroll,
        app.wrap_mode,
        is_focused,
        head_area,
        &app.theme,
        &app.highlight_cache,
        head_src,
        if is_focused {
            Some(app.diff_cursor)
        } else {
            None
        },
        app.selection_range(),
    );
}

/// Build unified diff lines by walking both lists in order.
/// Context lines appear once (labeled with the head line number so the
/// sequence is monotonic in head numbering); change blocks show removals
/// then additions.
pub(super) fn build_unified_lines(
    base_lines: &[LineChange],
    head_lines: &[LineChange],
) -> Vec<LineChange> {
    let mut unified = Vec::new();
    let mut bi = 0;
    let mut hi = 0;

    while bi < base_lines.len() || hi < head_lines.len() {
        let b_is_change = bi < base_lines.len() && base_lines[bi].1.starts_with('-');
        let h_is_change = hi < head_lines.len() && head_lines[hi].1.starts_with('+');

        if b_is_change || h_is_change {
            // Change block: all removals first, then all additions
            while bi < base_lines.len() && base_lines[bi].1.starts_with('-') {
                unified.push(base_lines[bi].clone());
                bi += 1;
            }
            while hi < head_lines.len() && head_lines[hi].1.starts_with('+') {
                unified.push(head_lines[hi].clone());
                hi += 1;
            }
        } else {
            // Context line — identical text in both sides. Carry the HEAD line
            // number (and advance both cursors) so the rendered sequence is
            // monotonic in head numbering; this lets the syntax highlighter be
            // primed from the head file's full text (multi-line constructs that
            // open above the hunk). Falls back to base when head is exhausted.
            if hi < head_lines.len() {
                unified.push(head_lines[hi].clone());
            } else if bi < base_lines.len() {
                unified.push(base_lines[bi].clone());
            }
            if bi < base_lines.len() {
                bi += 1;
            }
            if hi < head_lines.len() {
                hi += 1;
            }
        }
    }

    unified
}

fn render_unified_diff(f: &mut Frame, app: &App, area: Rect) {
    let current_file = match app.file_names.get(app.current_file_idx) {
        Some(f) => f,
        None => return,
    };
    let (base_lines, head_lines) = match app.file_changes.get(current_file) {
        Some(c) => c,
        None => return,
    };
    let scroll = *app.scroll_positions.get(current_file).unwrap_or(&0);
    let h_scroll = *app.h_scroll_positions.get(current_file).unwrap_or(&0);
    let is_focused = matches!(app.focused_pane, Pane::DiffContent);

    let unified_lines = build_unified_lines(base_lines, head_lines);

    // Unified interleaves both sides; prime from the head text (the "current"
    // file). `build_unified_lines` labels context and `+` lines with head
    // numbers, so those are forward-monotonic and index `head_src` correctly;
    // `-` lines carry base numbers and are simply highlighted in place.
    let head_src = app
        .full_content
        .get(current_file)
        .map(|(_, h)| h.as_slice());

    let title = format!("{} vs {}", app.left_label, app.right_label);
    render_diff_pane(
        f,
        &title,
        &unified_lines,
        current_file,
        scroll,
        h_scroll,
        app.wrap_mode,
        is_focused,
        area,
        &app.theme,
        &app.highlight_cache,
        head_src,
        if is_focused {
            Some(app.diff_cursor)
        } else {
            None
        },
        app.selection_range(),
    );
}

fn render_log_ui(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = Block::default()
        .title(Span::styled(
            " Commits ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focused));

    if app.commits.is_empty() {
        let empty = Paragraph::new(Span::styled("  No commits", Style::default().fg(t.fg_dim)))
            .block(block);
        f.render_widget(empty, area);
        return;
    }

    // Borders (2) + highlight symbol "▌ " (2)
    const CHROME_WIDTH: u16 = 4;
    let inner_width = area.width.saturating_sub(CHROME_WIDTH) as usize;

    let items: Vec<ListItem> = app
        .commits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let is_current = i == app.current_commit_idx;
            let subject_style = if is_current {
                Style::default()
                    .fg(t.fg_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg_normal)
            };

            // hash + " " + subject; truncate the subject (tail) so the hash
            // is always visible.
            let hash_w = UnicodeWidthStr::width(c.hash.as_str());
            let sep_w = 1;
            let max_subject = inner_width.saturating_sub(hash_w + sep_w);
            let subject = truncate_tail(&c.subject, max_subject);

            ListItem::new(Line::from(vec![
                Span::styled(c.hash.clone(), Style::default().fg(t.accent)),
                Span::styled(" ", Style::default()),
                Span::styled(subject, subject_style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(t.bg_selection))
        .highlight_symbol("\u{258c} ");

    f.render_stateful_widget(
        list,
        area,
        &mut ratatui::widgets::ListState::default().with_selected(Some(app.current_commit_idx)),
    );
}

/// Truncate `s` from the right with an ellipsis so it fits within `max_width`
/// display columns. Used for commit subjects where the start is most useful.
fn truncate_tail(s: &str, max_width: usize) -> String {
    let display_width = UnicodeWidthStr::width(s);
    if display_width <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "\u{2026}".to_string();
    }
    let target = max_width - 1;
    let mut width = 0;
    let mut end_byte = 0;
    for (idx, ch) in s.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        width += ch_width;
        end_byte = idx + ch.len_utf8();
    }
    format!("{}\u{2026}", &s[..end_byte])
}

fn render_rebase_notification(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    if let Some(notification) = &app.rebase_notification {
        let mut max_line_length = 0;
        let mut line_count = 0;
        for line in notification.lines() {
            max_line_length = max_line_length.max(line.len());
            line_count += 1;
        }
        let modal_width = (max_line_length as u16 + 6).min(70);
        let modal_height = (line_count as u16 + 6).min(16);
        let modal_area = centered_rect(modal_width, modal_height, area);

        // Dim the background behind the modal
        let dim_bg = Block::default().style(Style::default().bg(t.bg_modal_dim));
        f.render_widget(dim_bg, area);

        let background = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.accent))
            .style(Style::default().bg(t.bg_modal))
            .title(Span::styled(
                " Rebase Recommended ",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));

        f.render_widget(Clear, modal_area);
        f.render_widget(&background, modal_area);

        let inner_area = background.inner(modal_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(line_count as u16 + 2),
                Constraint::Length(3),
            ])
            .split(inner_area);

        let message = Paragraph::new(notification.clone())
            .style(Style::default().fg(t.fg_normal))
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(message, chunks[0]);

        let button_spans = vec![
            Span::styled(" r ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" Rebase now  ", Style::default().fg(t.fg_normal)),
            Span::styled(" i ", Style::default().fg(t.fg_badge).bg(t.fg_dim)),
            Span::styled(" Ignore", Style::default().fg(t.fg_normal)),
        ];
        let buttons = Paragraph::new(Line::from(button_spans)).alignment(Alignment::Center);
        f.render_widget(buttons, chunks[1]);
    }
}

fn render_help_modal(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let is_rebase = matches!(app.app_mode, AppMode::Rebase);
    let is_log = matches!(app.app_mode, AppMode::Log);

    let modal_width = 56u16;
    // Inner width excludes the two side borders; used to size separators.
    let inner_width = modal_width.saturating_sub(2) as usize;

    // Dim the background behind the modal
    let dim_bg = Block::default().style(Style::default().bg(t.bg_modal_dim));
    f.render_widget(dim_bg, area);

    let accent = t.accent;
    let fg_normal = t.fg_normal;
    let fg_bright = t.fg_bright;
    let bg_key_badge = t.bg_key_badge;
    let fg_separator = t.fg_separator;

    let section = |title: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled("  \u{25cf} ", Style::default().fg(accent)),
            Span::styled(
                title.to_owned(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
        ])
    };

    let sep = |w: usize| -> Line<'static> {
        Line::from(Span::styled(
            "\u{2500}".repeat(w),
            Style::default().fg(fg_separator),
        ))
    };

    let row = |key: &str, desc: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                format!(" {:^8} ", key),
                Style::default().fg(fg_bright).bg(bg_key_badge),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(desc.to_owned(), Style::default().fg(fg_normal)),
        ])
    };

    let empty = || -> Line<'static> { Line::from("") };

    let mut lines: Vec<Line<'static>> = vec![
        empty(),
        section("Navigation"),
        empty(),
        row("j / \u{2193}", "Move down / next item"),
        row("k / \u{2191}", "Move up / previous item"),
        row("PgDn", "Page down"),
        row("PgUp", "Page up"),
        row("Home", "Go to first"),
        row("End", "Go to last"),
        sep(inner_width),
    ];

    if is_rebase {
        lines.extend(vec![
            empty(),
            section("Rebase"),
            empty(),
            row("a", "Accept current change"),
            row("x", "Reject current change"),
            row("n", "Next file with changes"),
            row("p", "Previous file with changes"),
            row("c", "Commit accepted changes"),
            row("Esc", "Back to diff mode"),
            sep(inner_width),
            empty(),
            section("General"),
            empty(),
            row("?", "Toggle this help"),
        ]);
    } else if is_log {
        lines.extend(vec![
            empty(),
            section("Log"),
            empty(),
            row("Enter", "View diff of selected commit"),
            row("L", "Close log view"),
            row("Esc / q", "Back to diff mode"),
            sep(inner_width),
            empty(),
            section("General"),
            empty(),
            row("?", "Toggle this help"),
        ]);
    } else {
        lines.extend(vec![
            empty(),
            section("Diff View"),
            empty(),
            row("Tab", "Toggle focus (files / diff)"),
            row("h / \u{2190}", "Focus file list"),
            row("l / \u{2192}", "Focus diff content"),
            row("S-\u{2190}/\u{2192}", "Scroll diff horizontally"),
            row("u", "Toggle unified / side-by-side"),
            row("w", "Toggle word wrap"),
            row("v", "Start/stop line selection"),
            row("y", "Copy selection (or current line)"),
            row("R", "Hide pure renames from file list"),
            row("f", "Toggle full file / hunks view"),
            row("t", "Toggle dark / light theme"),
            row("T", "Toggle file tree / list view"),
            row("c", "Commit (AI-generated message)"),
            row("r", "Enter rebase mode"),
            row("s", "Sync (pull --rebase, then push)"),
            row("L", "Open commit log"),
            sep(inner_width),
            empty(),
            section("General"),
            empty(),
            row("q / Esc", "Quit"),
            row("?", "Toggle this help"),
        ]);
    }

    // Size the modal to its content (plus borders); centered_rect clamps to
    // the screen height so it never overflows on short terminals.
    let modal_height = lines.len() as u16 + 2;
    let modal_area = centered_rect(modal_width, modal_height, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_modal))
        .style(Style::default().bg(t.bg_modal))
        .title(Span::styled(
            " Keybindings ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(" ? ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" ", Style::default()),
            Span::styled(" Esc ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" to close ", Style::default().fg(t.fg_dim)),
        ]));

    f.render_widget(Clear, modal_area);
    f.render_widget(&block, modal_area);

    let inner = block.inner(modal_area);

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text).style(Style::default().bg(t.bg_modal));
    f.render_widget(paragraph, inner);
}

fn render_commit_modal(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let message = match &app.pending_commit_message {
        Some(m) => m.as_str(),
        None => return,
    };

    let modal_width = 78u16.min(area.width.saturating_sub(2));
    let lines: Vec<&str> = message.lines().collect();
    let line_count = lines.len().max(1) as u16;
    let modal_height = (line_count + 6).clamp(8, area.height.saturating_sub(2).max(8));
    let modal_area = centered_rect(modal_width, modal_height, area);

    let dim_bg = Block::default().style(Style::default().bg(t.bg_modal_dim));
    f.render_widget(dim_bg, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_modal))
        .style(Style::default().bg(t.bg_modal))
        .title(Span::styled(
            " Commit message ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(" Enter ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" commit  ", Style::default().fg(t.fg_dim)),
            Span::styled(" Esc ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" cancel ", Style::default().fg(t.fg_dim)),
        ]));

    f.render_widget(Clear, modal_area);
    f.render_widget(&block, modal_area);

    let inner = block.inner(modal_area);

    let body_lines: Vec<Line<'static>> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            // First line is the conventional-commits subject; highlight it.
            let style = if i == 0 {
                Style::default()
                    .fg(t.fg_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg_normal)
            };
            Line::from(Span::styled((*line).to_string(), style))
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(body_lines))
        .style(Style::default().bg(t.bg_modal))
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

fn render_remote_picker(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;

    let modal_width = 48u16;
    let line_count = app.remotes.len().max(1) as u16;
    let modal_height = (line_count + 6).min(area.height); // borders + title + footer + padding
    let modal_area = centered_rect(modal_width, modal_height, area);

    let dim_bg = Block::default().style(Style::default().bg(t.bg_modal_dim));
    f.render_widget(dim_bg, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_modal))
        .style(Style::default().bg(t.bg_modal))
        .title(Span::styled(
            " Choose remote to push to ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(" j/k ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" navigate  ", Style::default().fg(t.fg_dim)),
            Span::styled(" Enter ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" confirm  ", Style::default().fg(t.fg_dim)),
            Span::styled(" Esc ", Style::default().fg(t.fg_badge).bg(t.fg_key)),
            Span::styled(" cancel ", Style::default().fg(t.fg_dim)),
        ]));

    f.render_widget(Clear, modal_area);
    f.render_widget(&block, modal_area);

    let inner = block.inner(modal_area);

    let lines: Vec<Line<'static>> = if app.remotes.is_empty() {
        vec![Line::from(Span::styled(
            "  (no remotes)".to_string(),
            Style::default().fg(t.fg_dim),
        ))]
    } else {
        app.remotes
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let selected = i == app.current_remote_idx;
                let style = if selected {
                    Style::default()
                        .fg(t.fg_bright)
                        .bg(t.bg_key_badge)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.fg_normal)
                };
                let marker = if selected { " \u{25b6} " } else { "   " };
                Line::from(vec![
                    Span::styled(marker.to_string(), style),
                    Span::styled(name.clone(), style),
                ])
            })
            .collect()
    };

    let paragraph = Paragraph::new(Text::from(lines)).style(Style::default().bg(t.bg_modal));
    f.render_widget(paragraph, inner);
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    if r.width == 0 || r.height == 0 {
        return r;
    }

    let height = height.min(r.height);
    let width = width.min(r.width);

    let vert_margin = 100u16.saturating_sub(height * 100 / r.height) / 2;
    let horiz_margin = 100u16.saturating_sub(width * 100 / r.width) / 2;

    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(vert_margin),
            Constraint::Length(height),
            Constraint::Percentage(vert_margin),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(horiz_margin),
            Constraint::Length(width),
            Constraint::Percentage(horiz_margin),
        ])
        .split(popup_layout[1])[1]
}

fn render_help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let pairs: &[(&str, &str)] = match app.app_mode {
        AppMode::Diff => &[
            ("q", "Quit"),
            ("j/k", "Navigate"),
            ("Tab", "Focus"),
            ("h/l", "Panes"),
            ("u", "View"),
            ("w", "Wrap"),
            ("v", "Select"),
            ("y", "Copy"),
            ("t", "Theme"),
            ("c", "Commit"),
            ("r", "Rebase"),
            ("s", "Sync"),
            ("L", "Log"),
            ("?", "Help"),
        ],
        AppMode::Rebase => &[
            ("Esc", "Back"),
            ("j/k", "Navigate"),
            ("a", "Accept"),
            ("x", "Reject"),
            ("n/p", "Files"),
            ("c", "Commit"),
            ("?", "Help"),
        ],
        AppMode::Log => &[
            ("Esc", "Back"),
            ("j/k", "Navigate"),
            ("Enter", "Open"),
            ("L", "Close"),
            ("?", "Help"),
        ],
        AppMode::RemotePicker => &[("j/k", "Navigate"), ("Enter", "Confirm"), ("Esc", "Cancel")],
    };

    let mut spans: Vec<Span> = vec![Span::styled(" ", Style::default())];
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(t.border_dim)));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::default().fg(t.fg_key),
        ));
        spans.push(Span::styled(
            format!(" {}", desc),
            Style::default().fg(t.fg_dim),
        ));
    }

    let help = Paragraph::new(Line::from(spans)).style(Style::default().bg(t.bg_header));
    f.render_widget(help, area);

    // Overlay the transient status message right-aligned on the same line so
    // the keymap remains visible (e.g. when there are no files to diff).
    if let Some(msg) = &app.status_message {
        let is_error = msg.starts_with("Error");
        let color = if is_error { t.fg_removed } else { t.fg_added };
        let text = format!("{} ", msg);
        let width = UnicodeWidthStr::width(text.as_str()) as u16;
        if width > 0 && area.width > 0 {
            let w = width.min(area.width);
            let status_area = Rect {
                x: area.x + area.width - w,
                y: area.y,
                width: w,
                height: area.height,
            };
            let status = Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Right)
            .style(Style::default().bg(t.bg_header));
            f.render_widget(status, status_area);
        }
    }
}

/// Maximum display width of the code portion (excluding the pinned gutter) of
/// any diff line for `file`.
fn max_line_width(app: &App, file: &str) -> usize {
    let (base, head) = match app.file_changes.get(file) {
        Some(c) => c,
        None => return 0,
    };
    base.iter()
        .chain(head.iter())
        .filter(|(num, _)| *num != 0)
        .map(|(_, line)| {
            let content = line
                .strip_prefix('+')
                .or_else(|| line.strip_prefix('-'))
                .or_else(|| line.strip_prefix(' '))
                .unwrap_or(line);
            UnicodeWidthStr::width(content)
        })
        .max()
        .unwrap_or(0)
}

/// Adjust the current file's scroll so the diff cursor stays visible. Precise
/// in non-wrap mode (scroll indexes logical lines); skipped in wrap mode, where
/// scroll counts visual rows and the mapping is only approximate (best effort).
fn follow_cursor(app: &mut App, content_height: u16) {
    if !matches!(app.app_mode, AppMode::Diff) || app.wrap_mode {
        return;
    }
    let Some(file) = app.file_names.get(app.current_file_idx).cloned() else {
        return;
    };
    let visible = (content_height.saturating_sub(2)) as usize; // borders
    if visible == 0 {
        return;
    }
    let scroll = *app.scroll_positions.get(&file).unwrap_or(&0);
    let cur = app.diff_cursor;
    let new = if cur < scroll {
        cur
    } else if cur >= scroll + visible {
        cur + 1 - visible
    } else {
        scroll
    };
    if new != scroll {
        app.scroll_positions.insert(file, new);
    }
}

fn clamp_h_scroll(app: &mut App, content_area_width: u16) {
    const GUTTER_WIDTH: usize = 7;
    let file = match app.file_names.get(app.current_file_idx) {
        Some(f) => f.clone(),
        None => return,
    };

    let rest = content_area_width
        .saturating_sub((content_area_width as u32 * app.file_list_width_pct as u32 / 100) as u16);
    let pane_width = match app.view_mode {
        ViewMode::SideBySide => rest / 2,
        ViewMode::Unified => rest,
    };
    let inner = (pane_width.saturating_sub(2) as usize).saturating_sub(GUTTER_WIDTH);

    let widest = max_line_width(app, &file);
    let max_h = widest.saturating_sub(inner);
    let cur = app.h_scroll_positions.get(&file).copied().unwrap_or(0);
    if cur > max_h {
        app.h_scroll_positions.insert(file, max_h);
    }
}

fn clamp_scroll(app: &mut App, content_area_width: u16, content_area_height: u16) {
    let file = match app.file_names.get(app.current_file_idx) {
        Some(f) => f,
        None => return,
    };
    let (base, head) = match app.file_changes.get(file) {
        Some(c) => c,
        None => return,
    };

    let content_len = if app.wrap_mode {
        // Pane inner width = pane area width - 2 (borders).
        let rest = content_area_width.saturating_sub(
            (content_area_width as u32 * app.file_list_width_pct as u32 / 100) as u16,
        );
        let pane_width = match app.view_mode {
            ViewMode::SideBySide => rest / 2,
            ViewMode::Unified => rest,
        };
        let inner_width = pane_width.saturating_sub(2) as usize;
        match app.view_mode {
            ViewMode::SideBySide => aligned_wrapped_row_count(base, head, inner_width),
            ViewMode::Unified => unified_wrapped_row_count(base, head, inner_width),
        }
    } else {
        match app.view_mode {
            ViewMode::SideBySide => aligned_line_count(base, head),
            ViewMode::Unified => unified_line_count(base, head),
        }
    };

    let visible = content_area_height.saturating_sub(2) as usize;
    if content_len <= visible {
        app.scroll_positions.insert(file.clone(), 0);
        return;
    }
    let max_scroll = content_len - visible;
    let scroll = app.scroll_positions.get(file).copied().unwrap_or(0);
    if scroll > max_scroll {
        app.scroll_positions.insert(file.clone(), max_scroll);
    }
}

/// Build the styled stats spans (e.g. " +3 -1") and return their total display width.
fn build_file_stats<'a>(adds: usize, dels: usize, theme: &Theme) -> (Vec<Span<'a>>, usize) {
    if adds == 0 && dels == 0 {
        return (vec![], 0);
    }

    let mut spans = Vec::new();
    let mut width = 1; // leading space
    spans.push(Span::styled(" ", Style::default()));

    if adds > 0 {
        let s = format!("+{}", adds);
        width += UnicodeWidthStr::width(s.as_str());
        spans.push(Span::styled(s, Style::default().fg(theme.fg_added)));
    }
    if adds > 0 && dels > 0 {
        width += 1;
        spans.push(Span::styled(" ", Style::default()));
    }
    if dels > 0 {
        let s = format!("-{}", dels);
        width += UnicodeWidthStr::width(s.as_str());
        spans.push(Span::styled(s, Style::default().fg(theme.fg_removed)));
    }

    (spans, width)
}

/// Split a path into (filename, directory). Directory is "" for root-level
/// files. Uses '/' as the separator since git diff output is POSIX-style.
fn split_path_for_display(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name.to_string(), dir.to_string()),
        None => (path.to_string(), String::new()),
    }
}

/// Fit a (filename, directory) pair into `max_width` display columns,
/// VSCode-style: filename always visible, directory shown dimly after two
/// spaces, truncated from the left with `…` if the remainder is too small.
/// If the filename alone is wider than `max_width`, truncate the filename from
/// the right with `…` and drop the directory.
fn fit_file_and_dir(file: &str, dir: &str, max_width: usize) -> (String, String) {
    let file_w = UnicodeWidthStr::width(file);
    if file_w >= max_width {
        return (truncate_tail(file, max_width), String::new());
    }
    if dir.is_empty() {
        return (file.to_string(), String::new());
    }
    // 2 spaces between filename and directory
    let dir_budget = max_width - file_w - 2;
    if dir_budget == 0 {
        return (file.to_string(), String::new());
    }
    let dir_w = UnicodeWidthStr::width(dir);
    if dir_w <= dir_budget {
        return (file.to_string(), dir.to_string());
    }
    if dir_budget == 1 {
        return (file.to_string(), "\u{2026}".to_string());
    }
    let target = dir_budget - 1;
    let mut width = 0;
    let mut start_byte = dir.len();
    for (idx, ch) in dir.char_indices().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        width += ch_width;
        start_byte = idx;
    }
    (file.to_string(), format!("\u{2026}{}", &dir[start_byte..]))
}

/// Truncate a path from the left so it fits within `max_width` display columns,
/// preserving the filename (tail). Uses unicode display widths so East Asian
/// full-width characters are measured correctly.
#[allow(dead_code)]
fn truncate_path(path: &str, max_width: usize) -> String {
    let display_width = UnicodeWidthStr::width(path);
    if display_width <= max_width {
        return path.to_string();
    }
    if max_width <= 1 {
        return "\u{2026}".to_string();
    }
    // Reserve 1 column for the "…" prefix, keep as much of the tail as possible
    let target = max_width - 1;
    let mut width = 0;
    let mut start_byte = path.len();
    for (idx, ch) in path.char_indices().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        width += ch_width;
        start_byte = idx;
    }
    format!("\u{2026}{}", &path[start_byte..])
}

fn count_file_changes(app: &App, file: &str) -> (usize, usize) {
    if let Some((base, head)) = app.file_changes.get(file) {
        let dels = base.iter().filter(|(_, l)| l.starts_with('-')).count();
        let adds = head.iter().filter(|(_, l)| l.starts_with('+')).count();
        (adds, dels)
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_badge_text_per_case() {
        use crate::diff::RenameInfo;
        let t = Theme::dark();
        let pure = || {
            Some(RenameInfo {
                from: "old".to_string(),
                similarity: 100,
            })
        };
        let partial = || {
            Some(RenameInfo {
                from: "old".to_string(),
                similarity: 50,
            })
        };
        let text = |m: FileMeta| status_badge(Some(&m), &t).map(|(s, _)| s);

        // Status badges
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Deleted,
                rename: None,
            }),
            Some(" D")
        );
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Added,
                rename: None,
            }),
            Some(" A")
        );

        // Rename badges (status == Modified)
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Modified,
                rename: pure(),
            }),
            Some(" R")
        );
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Modified,
                rename: partial(),
            }),
            Some(" r")
        );

        // Plain modification: no badge
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Modified,
                rename: None,
            }),
            None
        );

        // Priority guard: status wins over a concurrent rename.
        assert_eq!(
            text(FileMeta {
                status: FileStatus::Deleted,
                rename: partial(),
            }),
            Some(" D")
        );
    }

    #[test]
    fn test_truncate_path_no_truncation_needed() {
        assert_eq!(truncate_path("src/main.rs", 20), "src/main.rs");
    }

    #[test]
    fn test_truncate_path_exact_fit() {
        assert_eq!(truncate_path("abcde", 5), "abcde");
    }

    #[test]
    fn test_truncate_path_truncates_from_left() {
        // 10 chars, max 6 → "…" + last 5 chars
        assert_eq!(truncate_path("abcdefghij", 6), "\u{2026}fghij");
    }

    #[test]
    fn test_truncate_path_very_narrow() {
        assert_eq!(truncate_path("abcdefghij", 1), "\u{2026}");
        assert_eq!(truncate_path("abcdefghij", 0), "\u{2026}");
    }

    #[test]
    fn test_truncate_path_width_2() {
        // max_width=2 → "…" + 1 char
        assert_eq!(truncate_path("abcdef", 2), "\u{2026}f");
    }

    #[test]
    fn test_truncate_path_cjk_characters() {
        // Each CJK char is 2 display columns wide
        // "日本語" = 6 columns, max 5 → "…" + "本語" (4 cols) = 5
        assert_eq!(truncate_path("日本語", 5), "\u{2026}本語");
    }

    #[test]
    fn test_truncate_path_mixed_ascii_cjk() {
        // "src/日本語.rs" — test that mixed content truncates correctly
        let path = "src/日本語.rs";
        let truncated = truncate_path(path, 8);
        // Should end with the tail that fits in 7 cols (8 - 1 for "…")
        assert!(truncated.starts_with('\u{2026}'));
        assert!(UnicodeWidthStr::width(truncated.as_str()) <= 8);
    }

    // ── build_file_stats ────────────────────────────────────────────────

    fn stats_content_width(spans: &[Span]) -> usize {
        spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum()
    }

    #[test]
    fn test_build_file_stats_no_changes() {
        let t = Theme::dark();
        let (spans, width) = build_file_stats(0, 0, &t);
        assert!(spans.is_empty());
        assert_eq!(width, 0);
    }

    #[test]
    fn test_build_file_stats_adds_only() {
        let t = Theme::dark();
        let (spans, width) = build_file_stats(42, 0, &t);
        // " +42" → 1 + 3 = 4
        assert_eq!(width, 4);
        assert_eq!(stats_content_width(&spans), width);
    }

    #[test]
    fn test_build_file_stats_dels_only() {
        let t = Theme::dark();
        let (spans, width) = build_file_stats(0, 7, &t);
        // " -7" → 1 + 2 = 3
        assert_eq!(width, 3);
        assert_eq!(stats_content_width(&spans), width);
    }

    #[test]
    fn test_build_file_stats_adds_and_dels() {
        let t = Theme::dark();
        let (spans, width) = build_file_stats(3, 1, &t);
        // " +3 -1" → 1 + 2 + 1 + 2 = 6
        assert_eq!(width, 6);
        assert_eq!(stats_content_width(&spans), width);
    }

    // ── wrap_rows / total_wrapped_rows ─────────────────────────────────

    #[test]
    fn test_wrap_rows_empty_line() {
        assert_eq!(wrap_rows(0, 80), 1);
    }

    #[test]
    fn test_wrap_rows_fits() {
        assert_eq!(wrap_rows(80, 80), 1);
        assert_eq!(wrap_rows(40, 80), 1);
    }

    #[test]
    fn test_wrap_rows_overflows() {
        assert_eq!(wrap_rows(81, 80), 2);
        assert_eq!(wrap_rows(160, 80), 2);
        assert_eq!(wrap_rows(161, 80), 3);
    }

    #[test]
    fn test_wrap_rows_zero_width_does_not_panic() {
        assert_eq!(wrap_rows(100, 0), 1);
    }

    #[test]
    fn test_total_wrapped_rows_gap_lines_collapse() {
        // Two gap rows = 2 rows total, regardless of pane width.
        let lines = vec![(0, String::new()), (0, String::new())];
        assert_eq!(total_wrapped_rows(&lines, 80), 2);
    }

    #[test]
    fn test_total_wrapped_rows_counts_gutter_in_wrap() {
        // Content stripped of '+' is 80 cols; gutter adds 7 → 87 cols total.
        // At pane width 80, that wraps to 2 visual rows.
        let content = format!("+{}", "x".repeat(80));
        let lines = vec![(1, content)];
        assert_eq!(total_wrapped_rows(&lines, 80), 2);
    }

    // ── pad_aligned_for_wrap ──────────────────────────────────────────

    #[test]
    fn test_pad_aligned_for_wrap_pads_shorter_side() {
        // Base line wraps to 2 rows at width=10 (gutter 7 + content 5 = 12 → 2),
        // head line wraps to 1 row at width=10 (gutter 7 + content 2 = 9 → 1).
        // Padding should add 1 blank entry on the head side.
        let base = vec![(1, " aaaaa".to_string())];
        let head = vec![(1, " bb".to_string())];
        let (b, h) = pad_aligned_for_wrap(base, head, 10);
        assert_eq!(b.len(), 1);
        assert_eq!(h.len(), 2);
        assert_eq!(h[1], (0, String::new()));
    }

    #[test]
    fn test_pad_aligned_for_wrap_no_op_when_equal() {
        let base = vec![(1, " short".to_string())];
        let head = vec![(1, " also".to_string())];
        let (b, h) = pad_aligned_for_wrap(base.clone(), head.clone(), 80);
        assert_eq!(b.len(), 1);
        assert_eq!(h.len(), 1);
    }

    // ── allocation-free wrapped row counters ─────────────────────────

    #[test]
    fn test_unified_wrapped_row_count_matches_unified_lines() {
        // Build a synthetic diff: 1 context + 1 removal + 1 addition.
        let base = vec![(1, " ctx".to_string()), (2, "-bye".to_string())];
        let head = vec![(1, " ctx".to_string()), (2, "+hi".to_string())];
        let unified = build_unified_lines(&base, &head);
        let alloc_total = total_wrapped_rows(&unified, 80);
        let free_total = unified_wrapped_row_count(&base, &head, 80);
        assert_eq!(alloc_total, free_total);
    }

    #[test]
    fn test_aligned_wrapped_row_count_matches_padded_total() {
        // For side-by-side: padded vector total rows should equal counter result.
        let base = vec![(1, " ctx".to_string()), (2, "-removed line".to_string())];
        let head = vec![(1, " ctx".to_string()), (2, "+added".to_string())];
        let (ab, ah) = align_lines(&base, &head);
        let (pb, ph) = pad_aligned_for_wrap(ab, ah, 20);
        let padded_total = total_wrapped_rows(&pb, 20).max(total_wrapped_rows(&ph, 20));
        let free_total = aligned_wrapped_row_count(&base, &head, 20);
        assert_eq!(padded_total, free_total);
    }

    #[test]
    fn test_build_file_stats_large_numbers() {
        let t = Theme::dark();
        let (spans, width) = build_file_stats(1000, 99999, &t);
        // " +1000 -99999" → 1 + 5 + 1 + 6 = 13
        assert_eq!(width, 13);
        assert_eq!(stats_content_width(&spans), width);
    }
}
