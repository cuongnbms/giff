use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

use super::theme::Theme;

pub static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
pub static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

fn to_ratatui_color(c: highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

pub fn highlight_code(code: &str, highlighter: &mut HighlightLines) -> Vec<Span<'static>> {
    match highlighter.highlight_line(code, &SYNTAX_SET) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| {
                Span::styled(
                    text.to_owned(),
                    Style::default().fg(to_ratatui_color(style.foreground)),
                )
            })
            .collect(),
        Err(_) => vec![Span::raw(code.to_owned())],
    }
}

fn highlight_code_with_bg(
    code: &str,
    highlighter: &mut HighlightLines,
    bg: Color,
) -> Vec<Span<'static>> {
    match highlighter.highlight_line(code, &SYNTAX_SET) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| {
                Span::styled(
                    text.to_owned(),
                    Style::default()
                        .fg(to_ratatui_color(style.foreground))
                        .bg(bg),
                )
            })
            .collect(),
        Err(_) => vec![Span::styled(code.to_owned(), Style::default().bg(bg))],
    }
}

#[allow(dead_code)]
pub fn highlight_line_changes(
    lines: &[(usize, String)],
    filename: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (gutter, content) = highlight_line_changes_split(lines, filename, theme);
    gutter
        .into_iter()
        .zip(content)
        .map(|(g, c)| {
            let mut spans = g.spans;
            spans.extend(c.spans);
            if spans.iter().all(|s| s.content.is_empty()) {
                return Line::from(Span::raw(""));
            }
            Line::from(spans)
        })
        .collect()
}

/// Like `highlight_line_changes` but returns gutter spans (line number + change
/// marker, fixed width) separately from the code content spans. Used by the
/// diff renderer to pin the gutter while the content scrolls horizontally.
pub fn highlight_line_changes_split(
    lines: &[(usize, String)],
    filename: &str,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let syntax = SYNTAX_SET
        .find_syntax_for_file(filename)
        .ok()
        .flatten()
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let fallback_name = if theme.is_dark {
        "base16-ocean.dark"
    } else {
        "base16-ocean.light"
    };
    let syn_theme = match THEME_SET.themes.get(&theme.syntax_theme) {
        Some(t) => t,
        None => match THEME_SET
            .themes
            .get(fallback_name)
            .or_else(|| THEME_SET.themes.values().next())
        {
            Some(t) => t,
            None => {
                let mut gutter = Vec::with_capacity(lines.len());
                let mut content = Vec::with_capacity(lines.len());
                for (num, line) in lines {
                    if *num == 0 {
                        gutter.push(Line::from(Span::raw("")));
                        content.push(Line::from(Span::raw("")));
                    } else {
                        gutter.push(Line::from(Span::raw(format!("{:4}   ", num))));
                        content.push(Line::from(Span::raw(line.to_owned())));
                    }
                }
                return (gutter, content);
            }
        },
    };
    let mut highlighter = HighlightLines::new(syntax, syn_theme);

    let fg_line_num = theme.fg_line_num;
    let bg_removed = theme.bg_removed;
    let bg_added = theme.bg_added;
    let fg_removed_marker = theme.fg_removed_marker;
    let fg_added_marker = theme.fg_added_marker;

    let mut gutter = Vec::with_capacity(lines.len());
    let mut content = Vec::with_capacity(lines.len());

    for (line_num, line) in lines {
        if *line_num == 0 {
            gutter.push(Line::from(Span::raw("")));
            content.push(Line::from(Span::raw("")));
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            gutter.push(Line::from(vec![
                Span::styled(
                    format!("{:4} ", line_num),
                    Style::default().fg(fg_line_num).bg(bg_removed),
                ),
                Span::styled(
                    "- ",
                    Style::default()
                        .fg(fg_removed_marker)
                        .bg(bg_removed)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            content.push(Line::from(highlight_code_with_bg(
                rest,
                &mut highlighter,
                bg_removed,
            )));
        } else if let Some(rest) = line.strip_prefix('+') {
            gutter.push(Line::from(vec![
                Span::styled(
                    format!("{:4} ", line_num),
                    Style::default().fg(fg_line_num).bg(bg_added),
                ),
                Span::styled(
                    "+ ",
                    Style::default()
                        .fg(fg_added_marker)
                        .bg(bg_added)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            content.push(Line::from(highlight_code_with_bg(
                rest,
                &mut highlighter,
                bg_added,
            )));
        } else {
            // Context lines in git diff format have a leading space; strip it
            // so code aligns with changed lines and indentation-sensitive
            // languages (Python, YAML) highlight correctly.
            let code = line.strip_prefix(' ').unwrap_or(line);
            gutter.push(Line::from(vec![
                Span::styled(format!("{:4} ", line_num), Style::default().fg(fg_line_num)),
                Span::styled("  ", Style::default()),
            ]));
            content.push(Line::from(highlight_code(code, &mut highlighter)));
        }
    }

    (gutter, content)
}
