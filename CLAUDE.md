# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build --release       # release build (LTO + single codegen unit)
cargo run -- [args]         # run locally; same flags as `giff` (see README)
cargo test                  # all tests
cargo test <name>           # filter to a single test (e.g. `cargo test resolve_defaults_to_dark`)
cargo fmt --check           # CI requires formatted code
cargo clippy -- -D warnings # CI fails on any clippy warning
```

CI (`.github/workflows/ci.yml`) runs fmt + clippy + `cargo build --release` + `cargo test` on Linux, macOS, and Windows. All four must pass.

## Architecture

`giff` is a TUI over `git diff`. The pipeline is: shell out to `git`, parse the unified-diff text, hold both sides in memory keyed by filename, and render with ratatui.

### Top-level modules

- **`src/main.rs`** — CLI parsing (clap), optional pre-flight `--auto-rebase`, decides which `DiffSource` to use, loads config + theme, hands control to `ui::run_app`.
- **`src/diff.rs`** — All git interaction (via `std::process::Command`) and unified-diff parsing. No UI types here.
- **`src/config.rs`** — Loads `~/.config/giff/config.toml` (XDG via `dirs` crate) and resolves a `Theme` from CLI flag → config file → default `dark`.
- **`src/ui/`** — TUI: terminal lifecycle (`mod.rs`), state types (`types.rs`), event loop (`event_loop.rs`), rendering (`render.rs`), rebase staging logic (`rebase.rs`), syntax highlighting bridge (`syntax.rs`), themes (`theme.rs`), tests (`tests.rs`).

### `DiffSource` is the central abstraction

`diff::DiffSource` (in `diff.rs`) enumerates every way the diff can be sourced: `Uncommitted`, `ToRef`, `Between`, `CustomArgs`, `Commit`. It implements `fetch()` returning `(FileChanges, left_label, right_label)`.

`FileChanges = HashMap<String, (Vec<LineChange>, Vec<LineChange>)>` — keyed by file path, with `(base_lines, head_lines)`. Each `LineChange = (line_number, content)` and content is prefixed with `+`/`-`/` ` from the parsed unified diff.

The current `DiffSource` is stored on `App` so the UI can re-fetch on filesystem changes or when toggling between Diff/Log views.

### App state and modes

`App` (in `src/ui/types.rs`) is the single source of UI truth — passed mutably through the event loop. `AppMode` switches the entire keymap and renderer:

- `Diff` — the default file-list + diff-pane view
- `Rebase` — accept/reject individual changes, then commit them back to working files
- `Log` — commit list; selecting a commit swaps `diff_source` to `DiffSource::Commit(hash)` (saved as `log_return_source` so Esc restores)
- `RemotePicker` — modal shown during `s` (sync) when multiple remotes exist

`Pane` (`FileList` vs `DiffContent`) controls which side receives `j`/`k` and scroll events within Diff mode. `ViewMode` toggles side-by-side vs unified rendering.

### Rebase staging

`ui::rebase::prepare_rebase_changes` walks `FileChanges` and **pairs consecutive deletions with consecutive additions** within each change block (first `-` with first `+`, second with second, etc.) to produce `Change` entries the user can accept/reject individually. Unpaired additions store `base_insert_pos` so they can be inserted at the right location in the base file. On commit, `event_loop::commit_rebase_changes` translates accepted `Change`s into `diff::ChangeOp` (`Replace` / `Delete` / `Insert`) and `diff::apply_changes` rewrites the file on disk. After applying, the app returns to `Diff` mode and the file watcher reloads the diff.

### Auto-reload via filesystem watcher

`ui::spawn_repo_watcher` uses `notify` to watch the repo root recursively. Events on `.git/objects/`, `.git/logs/`, `.git/info/`, `.git/FETCH_HEAD`, `.git/ORIG_HEAD`, and any `*.lock` file are filtered out — these are noisy during git operations and would cause reload storms. A surviving event sends `()` on a channel; the event loop drains it and re-runs `diff_source.fetch()`.

### Terminal hygiene

`ui::run_app` installs a panic hook that calls `restore_terminal()` (disable raw mode, leave alternate screen, disable mouse capture) **before** the original panic hook prints. Without this, a panic leaves the user's terminal unusable. Any new code paths that can panic during the UI lifetime must rely on this hook rather than ad-hoc cleanup.

### Themes

Built-in `Theme::dark()` and `Theme::light()` live in `src/ui/theme.rs`. User themes are defined via `[themes.<name>]` tables in `config.toml`; `ThemeConfig::to_theme` fills any missing fields from the `base` (defaults to `dark`). Resolution order: `--theme` flag → `theme = "..."` in config → `dark`. The UI also maintains a `theme_cycle` (initial + dark + light, deduped) so `t` can rotate between them at runtime.

Syntax highlighting is supplied by `syntect` (with `two-face` providing extra languages/themes). `is_valid_syntax_theme` warns on startup if the configured `syntax_theme` is missing — the UI falls back internally rather than crashing.

## Conventions to preserve

- **Don't add a git library.** Everything goes through `Command::new("git")` with `--no-color`. Parsing assumes that flag.
- **Diff parsing is regex-driven** (`DIFF_FILE_RE`, `HUNK_HEADER_RE`, `ANSI_ESCAPE_RE` as `LazyLock`). If you change diff invocation flags, re-check parsing.
- **Untracked files** are surfaced in `Uncommitted` mode by listing them via `git ls-files --others --exclude-standard` and synthesizing a diff per file (`diff_untracked_file`). They appear alongside tracked changes in `FileChanges`.
- Tests for UI behavior live in `src/ui/tests.rs`; tests for pure logic (config, diff parsing, apply_operations) sit in `#[cfg(test)] mod tests` blocks at the bottom of their module.
