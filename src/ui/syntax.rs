use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

use super::theme::Theme;

// Diff lines are fed to `highlight_line` with their trailing newline stripped
// (the unified-diff content carries no `\n`). The newline-aware syntax set
// relies on that `\n` to fire end-of-line context pops, so without it a comment
// or string context can leak into following lines — e.g. a `#` comment
// containing an apostrophe turns every subsequent line into one flat comment
// span. The `no_newlines` variant is built for exactly this line-without-`\n`
// feeding, so use it to keep multi-line constructs correct.
pub static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_no_newlines);
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
    let mut hl = LineHighlighter::new(filename, theme);
    let mut gutter = Vec::with_capacity(lines.len());
    let mut content = Vec::with_capacity(lines.len());
    for (num, line) in lines {
        let (g, c) = hl.next(*num, line);
        gutter.push(g);
        content.push(c);
    }
    (gutter, content)
}

/// A resumable, line-at-a-time syntax highlighter for diff lines. Holds the
/// syntect parse/highlight state so successive `next` calls continue the same
/// document — preserving multi-line constructs (block comments, strings) — and
/// lets the cache highlight a file incrementally as the user scrolls instead of
/// all at once. Bound to the `'static` global syntax/theme sets so it can be
/// stored across frames.
enum LineHighlighter {
    Syntect {
        // Boxed: HighlightLines is large, and boxing keeps the enum compact
        // (the common case is one variant per cached file).
        hl: Box<HighlightLines<'static>>,
        fg_line_num: Color,
        bg_removed: Color,
        bg_added: Color,
        fg_removed_marker: Color,
        fg_added_marker: Color,
    },
    /// Fallback when no syntect theme is available: render lines unstyled.
    Plain,
}

impl LineHighlighter {
    fn new(filename: &str, theme: &Theme) -> Self {
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
        let syn_theme = THEME_SET
            .themes
            .get(&theme.syntax_theme)
            .or_else(|| THEME_SET.themes.get(fallback_name))
            .or_else(|| THEME_SET.themes.values().next());
        match syn_theme {
            Some(t) => LineHighlighter::Syntect {
                hl: Box::new(HighlightLines::new(syntax, t)),
                fg_line_num: theme.fg_line_num,
                bg_removed: theme.bg_removed,
                bg_added: theme.bg_added,
                fg_removed_marker: theme.fg_removed_marker,
                fg_added_marker: theme.fg_added_marker,
            },
            None => LineHighlighter::Plain,
        }
    }

    /// Highlight one diff line, advancing parse state. `line_num == 0` marks a
    /// gap row (rendered empty, parse state untouched so it doesn't corrupt
    /// multi-line tracking).
    fn next(&mut self, line_num: usize, line: &str) -> (Line<'static>, Line<'static>) {
        match self {
            LineHighlighter::Plain => {
                if line_num == 0 {
                    (Line::from(Span::raw("")), Line::from(Span::raw("")))
                } else {
                    (
                        Line::from(Span::raw(format!("{:4}   ", line_num))),
                        Line::from(Span::raw(line.to_owned())),
                    )
                }
            }
            LineHighlighter::Syntect {
                hl,
                fg_line_num,
                bg_removed,
                bg_added,
                fg_removed_marker,
                fg_added_marker,
            } => {
                if line_num == 0 {
                    return (Line::from(Span::raw("")), Line::from(Span::raw("")));
                }
                if let Some(rest) = line.strip_prefix('-') {
                    let gutter = Line::from(vec![
                        Span::styled(
                            format!("{:4} ", line_num),
                            Style::default().fg(*fg_line_num).bg(*bg_removed),
                        ),
                        Span::styled(
                            "- ",
                            Style::default()
                                .fg(*fg_removed_marker)
                                .bg(*bg_removed)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]);
                    (
                        gutter,
                        Line::from(highlight_code_with_bg(rest, hl, *bg_removed)),
                    )
                } else if let Some(rest) = line.strip_prefix('+') {
                    let gutter = Line::from(vec![
                        Span::styled(
                            format!("{:4} ", line_num),
                            Style::default().fg(*fg_line_num).bg(*bg_added),
                        ),
                        Span::styled(
                            "+ ",
                            Style::default()
                                .fg(*fg_added_marker)
                                .bg(*bg_added)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]);
                    (
                        gutter,
                        Line::from(highlight_code_with_bg(rest, hl, *bg_added)),
                    )
                } else {
                    // Context lines in git diff format have a leading space;
                    // strip it so code aligns with changed lines and
                    // indentation-sensitive languages highlight correctly.
                    let code = line.strip_prefix(' ').unwrap_or(line);
                    let gutter = Line::from(vec![
                        Span::styled(
                            format!("{:4} ", line_num),
                            Style::default().fg(*fg_line_num),
                        ),
                        Span::styled("  ", Style::default()),
                    ]);
                    (gutter, Line::from(highlight_code(code, hl)))
                }
            }
        }
    }
}

/// Maximum number of distinct (line-set, theme) highlight results retained.
/// Side-by-side renders two panes per frame (base + head), so a handful of
/// slots covers the current file plus headroom for the previous one.
const HIGHLIGHT_CACHE_CAP: usize = 4;

struct HighlightEntry {
    key: u64,
    /// Resumable highlighter, kept so the file can be highlighted incrementally
    /// as the user scrolls. `None` once every line has been highlighted.
    hl: Option<LineHighlighter>,
    /// Number of source lines highlighted so far (`gutter`/`content` length).
    done: usize,
    gutter: Vec<Line<'static>>,
    content: Vec<Line<'static>>,
}

/// Memoizes diff syntax highlighting, computed lazily and incrementally.
///
/// Highlighting a whole file through syntect is expensive (hundreds of ms for
/// thousands of lines), and the diff is re-rendered on every input event. This
/// cache solves two problems at once:
///
/// - **No re-highlight on scroll.** Output depends only on content + theme, so
///   the same entry is reused across frames; scrolling never re-highlights.
/// - **No freeze on file switch.** Lines are highlighted on demand, only up to
///   the bottom of the visible window. Opening a file paints after highlighting
///   ~one screenful; scrolling extends the entry line-by-line. Because parsing
///   is continuous from line 0, multi-line constructs stay correct.
///
/// Entries are keyed by a content + theme hash, so any edit, file switch, or
/// theme change starts a fresh entry naturally.
#[derive(Default)]
pub struct HighlightCache {
    entries: Vec<HighlightEntry>,
    /// New-entry counter, used only by tests to verify that scrolling reuses an
    /// existing entry rather than re-highlighting from scratch.
    #[cfg(test)]
    misses: u64,
}

impl HighlightCache {
    /// Number of fresh entries created (full re-highlights). Exposed for tests.
    #[cfg(test)]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Ensure the entry for `(lines, filename, theme)` exists and has at least
    /// `upto` source lines highlighted, then return its index. Highlighting is
    /// resumed from where it left off, so each call does work proportional to
    /// the newly requested lines, not the whole file.
    fn ensure_upto(
        &mut self,
        lines: &[(usize, String)],
        filename: &str,
        theme: &Theme,
        upto: usize,
    ) -> usize {
        let key = cache_key(lines, filename, theme);
        let idx = match self.entries.iter().position(|e| e.key == key) {
            Some(i) => i,
            None => {
                #[cfg(test)]
                {
                    self.misses += 1;
                }
                if self.entries.len() >= HIGHLIGHT_CACHE_CAP {
                    self.entries.remove(0);
                }
                self.entries.push(HighlightEntry {
                    key,
                    hl: Some(LineHighlighter::new(filename, theme)),
                    done: 0,
                    gutter: Vec::new(),
                    content: Vec::new(),
                });
                self.entries.len() - 1
            }
        };

        let e = &mut self.entries[idx];
        let target = upto.min(lines.len());
        if e.done < target {
            if let Some(hl) = e.hl.as_mut() {
                for (num, line) in &lines[e.done..target] {
                    let (g, c) = hl.next(*num, line);
                    e.gutter.push(g);
                    e.content.push(c);
                }
                e.done = target;
                if e.done == lines.len() {
                    e.hl = None; // fully highlighted; drop parser state
                }
            }
        }
        idx
    }

    /// Clone the `[start, start+len)` window of highlighted lines, highlighting
    /// up to that point on demand. Each frame copies only the visible rows, so
    /// scrolling is O(visible). `start`/`len` are clamped to the line count.
    pub fn window(
        &mut self,
        lines: &[(usize, String)],
        filename: &str,
        theme: &Theme,
        start: usize,
        len: usize,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let idx = self.ensure_upto(lines, filename, theme, start.saturating_add(len));
        let e = &self.entries[idx];
        let total = e.content.len();
        let s = start.min(total);
        let end = start.saturating_add(len).min(total);
        (e.gutter[s..end].to_vec(), e.content[s..end].to_vec())
    }
}

/// Hash the inputs that determine highlight output. Theme is reduced to the
/// fields the highlighter actually reads (syntect theme + the gutter/marker
/// colors baked into the spans).
fn cache_key(lines: &[(usize, String)], filename: &str, theme: &Theme) -> u64 {
    let mut h = DefaultHasher::new();
    filename.hash(&mut h);
    theme.syntax_theme.hash(&mut h);
    theme.is_dark.hash(&mut h);
    theme.bg_added.hash(&mut h);
    theme.bg_removed.hash(&mut h);
    theme.fg_line_num.hash(&mut h);
    theme.fg_added_marker.hash(&mut h);
    theme.fg_removed_marker.hash(&mut h);
    lines.len().hash(&mut h);
    for (num, line) in lines {
        num.hash(&mut h);
        line.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `#` comment containing an apostrophe must not leak its context into
    /// following lines. Regression test for the newline-handling bug: with a
    /// newline-requiring syntax set and lines fed without a trailing `\n`, the
    /// comment's end-of-line context pop never fired and every subsequent line
    /// collapsed to a single comment-colored span.
    #[test]
    fn comment_with_apostrophe_does_not_bleed_into_next_lines() {
        let theme = Theme::dark();
        let lines: Vec<(usize, String)> = vec![
            (
                1,
                "+# GitPython's Repo() expands paths, but mkdir() does not".to_string(),
            ),
            (
                2,
                "+GITOPS_CLONE_PATH = os.path.expanduser(env('PATH', default='/data'))".to_string(),
            ),
            (3, "+GITOPS_REPO_URL = env(".to_string()),
        ];
        let (_g, content) = highlight_line_changes_split(&lines, "gitops.py", &theme);

        // The two code lines after the comment must be tokenized into multiple
        // distinctly-colored spans, not rendered as one flat run.
        for idx in [1usize, 2] {
            let spans = &content[idx].spans;
            let distinct: std::collections::HashSet<_> = spans.iter().map(|s| s.style.fg).collect();
            assert!(
                distinct.len() > 1,
                "line {idx} should have multiple token colors, got {} span(s) all fg={:?}: {:?}",
                spans.len(),
                spans.first().map(|s| s.style.fg),
                spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>(),
            );
        }
    }

    fn sample_lines() -> Vec<(usize, String)> {
        vec![
            (1, " fn main() {".to_string()),
            (2, "-    old();".to_string()),
            (3, "+    new();".to_string()),
            (4, " }".to_string()),
        ]
    }

    #[test]
    fn scrolling_reuses_highlight_and_returns_window() {
        let theme = Theme::dark();
        let lines = sample_lines();
        let mut cache = HighlightCache::default();

        // First frame highlights the file; subsequent scroll frames must not.
        let full = cache.window(&lines, "main.rs", &theme, 0, lines.len());
        let win = cache.window(&lines, "main.rs", &theme, 1, 2);

        assert_eq!(cache.misses(), 1, "scrolling must not re-highlight");
        // The window is exactly the requested slice of the full result.
        assert_eq!(win.0, full.0[1..3].to_vec());
        assert_eq!(win.1, full.1[1..3].to_vec());
    }

    #[test]
    fn highlighting_is_incremental_and_continuous() {
        let theme = Theme::dark();
        let lines = sample_lines(); // 4 lines
        let mut cache = HighlightCache::default();

        // A file switch only needs the first screenful highlighted.
        let top = cache.window(&lines, "main.rs", &theme, 0, 2);
        // Scrolling down extends the SAME entry rather than re-highlighting.
        let more = cache.window(&lines, "main.rs", &theme, 0, 4);
        assert_eq!(cache.misses(), 1, "extending must reuse the entry");

        // Incremental output must be identical to a single full-file pass,
        // i.e. parse state is continuous across the on-demand chunks.
        let full = highlight_line_changes_split(&lines, "main.rs", &theme);
        assert_eq!(top.1, full.1[0..2].to_vec());
        assert_eq!(more.1, full.1[0..4].to_vec());
    }

    #[test]
    fn window_clamps_out_of_range_bounds() {
        let theme = Theme::dark();
        let lines = sample_lines(); // 4 lines
        let mut cache = HighlightCache::default();

        let win = cache.window(&lines, "main.rs", &theme, 3, 100);
        assert_eq!(win.1.len(), 1, "len clamps to available lines");

        let past_end = cache.window(&lines, "main.rs", &theme, 10, 5);
        assert!(past_end.1.is_empty(), "start past end yields empty window");
    }

    #[test]
    fn cache_misses_when_content_or_theme_changes() {
        let lines = sample_lines();
        let mut cache = HighlightCache::default();

        let n = lines.len();
        cache.window(&lines, "main.rs", &Theme::dark(), 0, n);
        let mut edited = lines.clone();
        edited[2].1 = "+    newer();".to_string();
        cache.window(&edited, "main.rs", &Theme::dark(), 0, n);
        cache.window(&lines, "main.rs", &Theme::light(), 0, n);

        assert_eq!(cache.misses(), 3, "edits and theme changes must recompute");
    }
}
