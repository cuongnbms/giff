<div align="center">

# giff

**A terminal UI for git diffs with interactive rebase, sync, log browsing, and AI commits.**

[![CI](https://github.com/bahdotsh/giff/actions/workflows/ci.yml/badge.svg)](https://github.com/bahdotsh/giff/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/giff.svg)](https://crates.io/crates/giff)
[![License](https://img.shields.io/crates/l/giff.svg)](LICENSE)

![giff demo](assets/demo.gif)

</div>

---

## Features

- **Side-by-side & unified diffs** — toggle between layouts with a single key
- **Syntax highlighting** — language-aware coloring for 130+ languages via syntect
- **Full-file view** — flip between hunks-only and the entire file with surrounding context
- **Horizontal scrolling** — pan long lines with `Shift+←/→` or trackpad swipe; the line-number gutter stays pinned
- **Resizable file pane** — drag the divider between the file list and diff pane with the mouse
- **Branch status in header** — current branch plus ahead/behind counts vs upstream
- **Auto-reload** — file system watcher refreshes the diff when the working tree changes
- **Untracked files** — surfaced alongside tracked changes in uncommitted mode
- **Interactive rebase** — accept or reject individual hunks, then commit the result back to disk
- **Rebase detection** — notifies you when your branch is behind or has diverged
- **Sync** — `pull --rebase` then `push` (with a remote picker when none is set as upstream)
- **Commit log browser** — open the log, jump into any commit's diff, jump back
- **AI commit messages** — generate a Conventional Commits message from the staged diff via the local `claude` CLI, then confirm before committing
- **Dark & light themes** — built-in themes with full customization through config; press `t` to cycle
- **Vim-style navigation** — keyboard-first with mouse scroll support
- **Help overlay** — press `?` anywhere to see all keybindings in context

### Side-by-side & syntax highlighting

![Rust syntax highlighting](assets/syntax-highlight.png)

### Unified view

![Unified diff view](assets/unified-view.png)

### Light theme

![Light theme](assets/light-theme.png)

### Help overlay

![Help overlay](assets/help-overlay.png)

## Install

```bash
cargo install giff
```

Or build from source:

```bash
git clone https://github.com/bahdotsh/giff.git
cd giff && cargo build --release
```

### Optional: AI commit messages

The `c` keybinding in Diff mode invokes the [`claude`](https://docs.claude.com/en/docs/claude-code/overview) CLI to draft a Conventional Commits message from the staged diff. If `claude` isn't on your `PATH`, every other feature still works — only this one is disabled.

## Usage

```bash
giff                        # uncommitted changes vs HEAD (includes untracked files)
giff main feature-branch    # diff between two refs
giff main                   # diff ref against working tree
giff --theme light          # override theme
giff -d "--stat"            # pass custom git diff args
giff --auto-rebase          # auto-rebase if behind upstream, then open the UI
```

## Keybindings

### Diff mode

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate down / up |
| `PgDn` / `PgUp` | Page down / up |
| `Home` / `End` | Jump to first / last item |
| `Tab` | Toggle focus between file list and diff |
| `h` / `l` / `←` / `→` | Focus file list / diff content |
| `Shift+←` / `Shift+→` | Scroll diff horizontally (for long lines) |
| `u` | Toggle unified / side-by-side view |
| `f` | Toggle full-file / hunks-only view |
| `t` | Cycle themes (initial → dark → light) |
| `c` | Commit with an AI-generated message (requires the `claude` CLI) |
| `r` | Enter rebase mode |
| `s` | Sync (`pull --rebase`, then `push`) |
| `L` | Open the commit log |
| `?` | Show help |
| `q` / `Esc` | Quit (or back out of a commit's diff if opened from the log) |

### Rebase mode

| Key | Action |
|---|---|
| `j` / `k` | Next / previous change |
| `a` / `x` | Accept / reject change |
| `n` / `p` | Next / previous file with changes |
| `c` | Commit accepted changes (writes them back to the working tree) |
| `?` | Show help |
| `Esc` | Cancel rebase |

### Log mode

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Next / previous commit |
| `PgDn` / `PgUp` / `Home` / `End` | Page / jump |
| `Enter` | Open the selected commit's diff |
| `L` / `Esc` / `q` | Return to the previous diff view |

### Commit confirmation modal

| Key | Action |
|---|---|
| `Enter` / `y` | Stage everything and commit with the generated message |
| `Esc` / `n` / `q` | Cancel without committing |

### Remote picker (shown when sync has no configured upstream)

| Key | Action |
|---|---|
| `j` / `k` | Next / previous remote |
| `Enter` | Push to the selected remote (sets upstream) |
| `Esc` | Cancel |

### Mouse

- Scroll wheel works in both the file list and diff panes.
- Two-finger horizontal swipe scrolls the diff sideways.
- Drag the vertical divider between the file list and diff pane to resize.

## Configuration

`~/.config/giff/config.toml`

```toml
theme = "your_custom_theme_name"

[themes.your_custom_theme_name]
base = "dark"
accent = "#89b4fa"
fg_added = "#a6e3a1"
fg_removed = "#f38ba8"
```

See the built-in dark and light themes in [`src/ui/theme.rs`](src/ui/theme.rs) for all available color keys.

## Contributing

Contributions welcome — feel free to open an issue or submit a PR.

## License

[MIT](LICENSE) or [Unlicense](LICENSE), at your option.
