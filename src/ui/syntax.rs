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

    /// Advance parse state over one raw source line without producing output.
    /// Used to prime the highlighter with the file content preceding (or
    /// skipped between) the diff lines, so multi-line constructs that open
    /// outside the diff (docstrings, block comments) are tracked correctly.
    fn prime(&mut self, raw: &str) {
        if let LineHighlighter::Syntect { hl, .. } = self {
            let _ = hl.highlight_line(raw, &SYNTAX_SET);
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

/// Upper bound on how many preceding source lines are fed to the highlighter
/// to recover parse state before a hunk. Priming costs roughly one
/// highlight-per-line, so this caps the worst-case first-paint work when a
/// change sits deep in a large file. Comfortably covers real-world banners,
/// license headers, and docstrings; a multi-line construct spanning more than
/// this many lines degrades to the (pre-fix) unprimed behavior.
const MAX_PRIME_LINES: usize = 2000;

struct HighlightEntry {
    key: u64,
    /// Resumable highlighter, kept so the file can be highlighted incrementally
    /// as the user scrolls. `None` once every line has been highlighted.
    hl: Option<LineHighlighter>,
    /// Number of source lines highlighted so far (`gutter`/`content` length).
    done: usize,
    /// Highest source line number whose parse state has been consumed (via a
    /// diff line or priming). Lets `ensure_upto` feed only the gap before the
    /// next diff line, and never rewind on out-of-order line numbers.
    fed_upto: usize,
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
        source: Option<&[String]>,
    ) -> usize {
        let key = cache_key(lines, filename, theme, source);
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
                    fed_upto: 0,
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
                    // Before highlighting a diff line, feed the source lines
                    // that precede it but were elided from the diff, so parse
                    // state (open strings / block comments) is correct. Only
                    // when the line number advances past what's already been
                    // consumed; out-of-order numbers (unified `-` lines, gap
                    // rows) never rewind state. The fed span is capped so a
                    // change deep in a huge file can't freeze the first paint.
                    if *num > e.fed_upto + 1 {
                        if let Some(src) = source {
                            let to = (*num - 1).min(src.len()); // exclusive, 0-based
                            let from = e.fed_upto.max(to.saturating_sub(MAX_PRIME_LINES));
                            if from < to {
                                for raw in &src[from..to] {
                                    hl.prime(raw);
                                }
                            }
                        }
                    }
                    let (g, c) = hl.next(*num, line);
                    e.gutter.push(g);
                    e.content.push(c);
                    if *num > e.fed_upto {
                        e.fed_upto = *num;
                    }
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
    /// Convenience wrapper around `window_src` with no priming source. Used by
    /// tests and benchmarks; the render path always calls `window_src`.
    #[cfg(test)]
    pub fn window(
        &mut self,
        lines: &[(usize, String)],
        filename: &str,
        theme: &Theme,
        start: usize,
        len: usize,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.window_src(lines, filename, theme, start, len, None)
    }

    /// Like `window`, but `source` is the full file text for the rendered
    /// side (one entry per line, contiguous from line 1). When present, parse
    /// state is primed from it so multi-line constructs opened above the
    /// visible hunk are highlighted correctly.
    pub fn window_src(
        &mut self,
        lines: &[(usize, String)],
        filename: &str,
        theme: &Theme,
        start: usize,
        len: usize,
        source: Option<&[String]>,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let idx = self.ensure_upto(lines, filename, theme, start.saturating_add(len), source);
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
fn cache_key(
    lines: &[(usize, String)],
    filename: &str,
    theme: &Theme,
    source: Option<&[String]>,
) -> u64 {
    let mut h = DefaultHasher::new();
    // Priming changes the produced colors, so an entry highlighted without a
    // source must not be reused once one is available. The line count is a
    // cheap, sufficient fingerprint: the source is deterministic from the
    // diff state, which already changes `lines` (and thus the key) on edits.
    source.map(|s| s.len()).hash(&mut h);
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

    /// A hunk whose lines sit inside a triple-quoted docstring opened ABOVE
    /// the diff must be highlighted as a string once the preceding file lines
    /// are supplied as priming source — not as loose comments/code.
    #[test]
    fn priming_source_fixes_construct_opened_above_the_hunk() {
        let theme = Theme::dark();
        // Diff shows only lines 4-5; the `"""` opened at line 1 is off-diff.
        let lines: Vec<(usize, String)> = vec![
            (
                4,
                "     # not a real comment, this is inside a docstring".to_string(),
            ),
            (5, "+    python tool.py --apply --select 1,3,6".to_string()),
        ];
        // Full head text: the docstring opens at line 1 and stays open.
        let source: Vec<String> = vec![
            "\"\"\"".to_string(),
            "Module docstring.".to_string(),
            "".to_string(),
            "    # not a real comment, this is inside a docstring".to_string(),
            "    python tool.py --apply --select 1,3,6".to_string(),
        ];

        // Without priming (today's behavior): the highlighter starts at line 4
        // ignorant of the open string, so it tokenizes the lines as code.
        let mut bare = HighlightCache::default();
        let (_g, unprimed) = bare.window(&lines, "tool.py", &theme, 0, lines.len());
        let unprimed_colors: std::collections::HashSet<_> = unprimed
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.style.fg))
            .collect();

        // With priming: every rendered line is inside the open string, so each
        // collapses to a single uniform string color.
        let mut primed = HighlightCache::default();
        let (_g, content) =
            primed.window_src(&lines, "tool.py", &theme, 0, lines.len(), Some(&source));
        for (i, c) in content.iter().enumerate() {
            let colors: std::collections::HashSet<_> = c.spans.iter().map(|s| s.style.fg).collect();
            assert_eq!(
                colors.len(),
                1,
                "primed docstring line {i} should be one color, got {colors:?}",
            );
        }

        // The fix must actually change the output, else the test proves nothing.
        let primed_colors: std::collections::HashSet<_> = content
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.style.fg))
            .collect();
        assert_ne!(
            unprimed_colors, primed_colors,
            "priming must change the highlighting vs the unprimed path",
        );
    }

    /// A multi-line construct that opens AND closes entirely within a
    /// between-hunk gap must not leak: priming has to feed the whole gap (up to
    /// the next diff line) so the construct's close fires. This is the unified
    /// failure mode — a function docstring sitting between two hunks left the
    /// parser mid-string and painted the following code as one flat color.
    #[test]
    fn priming_feeds_entire_between_hunk_gap() {
        let theme = Theme::dark();
        // Diff shows head lines 1 and 6; lines 2-5 are an elided gap that opens
        // a docstring (line 3) and closes it (line 5).
        let lines: Vec<(usize, String)> = vec![
            (1, " def f():".to_string()),
            (6, "+    result = compute(a, b)".to_string()),
        ];
        let source: Vec<String> = vec![
            "def f():".to_string(),
            "    x = 1".to_string(),
            "    s = \"\"\"".to_string(),
            "    still inside the string".to_string(),
            "    end\"\"\"".to_string(),
            "    result = compute(a, b)".to_string(),
        ];
        let mut cache = HighlightCache::default();
        let (_g, content) = cache.window_src(&lines, "f.py", &theme, 0, lines.len(), Some(&source));
        // The post-gap code line must be tokenized as code (multiple colors),
        // proving the docstring opened at line 3 was closed at line 5 during
        // priming rather than leaking into line 6.
        let colors: std::collections::HashSet<_> =
            content[1].spans.iter().map(|s| s.style.fg).collect();
        assert!(
            colors.len() > 1,
            "post-gap code should be multi-colored, got {colors:?}",
        );
    }

    /// Out-of-order line numbers (unified `-` lines, alignment gap rows) must
    /// not trigger a state-corrupting rewind or panic when a source is present.
    #[test]
    fn priming_tolerates_out_of_order_and_gap_rows() {
        let theme = Theme::dark();
        let lines: Vec<(usize, String)> = vec![
            (10, "+let x = 1;".to_string()),
            (0, " ".to_string()),             // alignment gap row
            (3, "-let old = 2;".to_string()), // base-numbered line, < fed_upto
            (11, "+let y = 3;".to_string()),
        ];
        let source: Vec<String> = (1..=11).map(|n| format!("line {n}")).collect();
        let mut cache = HighlightCache::default();
        // Must not panic; window length matches the request.
        let (_g, content) = cache.window_src(&lines, "f.rs", &theme, 0, lines.len(), Some(&source));
        assert_eq!(content.len(), lines.len());
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
