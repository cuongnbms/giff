use regex::Regex;
use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

static DIFF_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^diff --git a/(.+) b/(.+)$").unwrap());
static HUNK_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@ -(\d+),?\d* \+(\d+),?\d* @@").unwrap());
static ANSI_ESCAPE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[.*?m").unwrap());
static SIMILARITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^similarity index (\d+)%$").unwrap());

pub type LineChange = (usize, String);
pub type FileChanges = HashMap<String, (Vec<LineChange>, Vec<LineChange>)>;
pub type FileMetaMap = HashMap<String, FileMeta>;

/// Rename details captured from `git diff`'s `rename from`/`rename to` and
/// `similarity index N%` headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameInfo {
    pub from: String,
    /// `0..=100`. A pure rename (no content changes) is reported as 100%.
    pub similarity: u8,
}

/// Per-file metadata that lives alongside `FileChanges`. Currently just
/// rename info, but kept as a struct so additional fields (mode change,
/// binary, etc.) can land here without churning every call site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    pub rename: Option<RenameInfo>,
}

impl FileMeta {
    /// `true` when git reported this file as a 100%-similarity rename, i.e.
    /// a name change with no content delta.
    pub fn is_pure_rename(&self) -> bool {
        matches!(&self.rename, Some(r) if r.similarity >= 100)
    }

    /// `true` when git reported any rename (pure or partial).
    pub fn is_rename(&self) -> bool {
        self.rename.is_some()
    }
}

/// Bundle returned by every `DiffSource::fetch*` call: the file diffs, the
/// per-file metadata, and the labels for the left/right panes.
pub struct DiffPayload {
    pub files: FileChanges,
    pub meta: FileMetaMap,
    pub left_label: String,
    pub right_label: String,
}

/// How to (re-)fetch the diff. Captures the user's CLI selection so the
/// running UI can refresh itself without re-parsing argv.
#[derive(Clone, Debug, PartialEq)]
pub enum DiffSource {
    Uncommitted,
    ToRef(String),
    Between {
        from: String,
        to: String,
    },
    CustomArgs(String),
    Commit(String),
    /// Diff the current branch against its fork point (`git merge-base
    /// <base> HEAD`), shown against the working tree. `None` resolves the
    /// base lazily at fetch time (upstream, else `main`/`master`).
    SinceFork {
        base: Option<String>,
    },
}

impl DiffSource {
    /// Fetch the diff with git's default context (3 lines).
    pub fn fetch(&self) -> Result<DiffPayload, Box<dyn Error>> {
        self.fetch_with_context(None)
    }

    /// Fetch with an explicit context size. `None` means git's default;
    /// `Some(n)` passes `--unified=n` (use a huge value for full-file view).
    pub fn fetch_with_context(
        &self,
        context: Option<usize>,
    ) -> Result<DiffPayload, Box<dyn Error>> {
        match self {
            DiffSource::Uncommitted => get_uncommitted_changes(context),
            DiffSource::ToRef(r) => get_changes_to_ref(r, context),
            DiffSource::Between { from, to } => get_changes_between(from, to, context),
            DiffSource::CustomArgs(a) => get_changes_with_args(a, context),
            DiffSource::Commit(h) => get_changes_for_commit(h, context),
            DiffSource::SinceFork { base } => get_changes_since_fork(base.as_deref(), context),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommitInfo {
    pub hash: String,
    pub subject: String,
}

pub fn get_commit_log() -> Result<Vec<CommitInfo>, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["log", "--pretty=format:%h %s", "--no-color"])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to execute git log: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, ' ');
        let hash = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        if !hash.is_empty() {
            commits.push(CommitInfo { hash, subject });
        }
    }
    Ok(commits)
}

pub fn get_changes_for_commit(
    hash: &str,
    context: Option<usize>,
) -> Result<DiffPayload, Box<dyn Error>> {
    let cmd_args = build_git_args(
        "show",
        &["--format=", "-m", "--first-parent", hash],
        context,
    );
    let output = Command::new("git").args(&cmd_args).output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to execute git show: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

    let (files, meta) = parse_diff_output(&stdout)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: format!("{}^", hash),
        right_label: hash.to_string(),
    })
}

// Get changes with completely custom diff args
pub fn get_changes_with_args(
    args: &str,
    context: Option<usize>,
) -> Result<DiffPayload, Box<dyn Error>> {
    let args_vec: Vec<&str> = args.split_whitespace().collect();
    let diff_output = get_diff_output_with_args(&args_vec, context)?;

    let (files, meta) = parse_diff_output(&diff_output)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: extract_left_label(args),
        right_label: extract_right_label(args),
    })
}

// Compare uncommitted changes (git diff)
pub fn get_uncommitted_changes(context: Option<usize>) -> Result<DiffPayload, Box<dyn Error>> {
    let mut diff_output = get_diff_output_with_args(&[], context)?;

    // `git diff` omits untracked files. Synthesize a diff against /dev/null
    // for each so newly added files render alongside modifications.
    if let Ok(repo_root) = git_repo_root() {
        for file in list_untracked_files(&repo_root) {
            if let Some(extra) = diff_untracked_file(&repo_root, &file) {
                diff_output.push_str(&extra);
            }
        }
    }

    let (files, meta) = parse_diff_output(&diff_output)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: "HEAD".to_string(),
        right_label: "Working Tree".to_string(),
    })
}

fn list_untracked_files(repo_root: &str) -> Vec<String> {
    let output = match Command::new("git")
        .current_dir(repo_root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn diff_untracked_file(repo_root: &str, file: &str) -> Option<String> {
    // `git diff --no-index` exits with code 1 when files differ; ignore status.
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["diff", "--no-color", "--no-index", "--", "/dev/null", file])
        .output()
        .ok()?;
    Some(String::from_utf8(output.stdout).unwrap_or_default())
}

// Compare a specific reference to working tree (git diff <ref>)
pub fn get_changes_to_ref(
    reference: &str,
    context: Option<usize>,
) -> Result<DiffPayload, Box<dyn Error>> {
    let diff_output = get_diff_output_with_args(&[reference], context)?;
    let (files, meta) = parse_diff_output(&diff_output)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: reference.to_string(),
        right_label: "Working Tree".to_string(),
    })
}

/// Compare the current branch against its fork point: `git merge-base
/// <base> HEAD`, diffed against the working tree. `base = None` resolves
/// the default base via [`default_fork_base`].
pub fn get_changes_since_fork(
    base: Option<&str>,
    context: Option<usize>,
) -> Result<DiffPayload, Box<dyn Error>> {
    let base_ref = match base {
        Some(b) => b.to_string(),
        None => default_fork_base()?,
    };
    let fork = merge_base(&base_ref, "HEAD")?;
    // Git object hashes are ASCII hex, so byte slicing is char-boundary safe.
    let short = fork.get(..8).unwrap_or(&fork);
    let diff_output = get_diff_output_with_args(&[&fork], context)?;
    let (files, meta) = parse_diff_output(&diff_output)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: format!("{} (fork: {})", base_ref, short),
        right_label: "Working Tree".to_string(),
    })
}

/// Resolve the default base for fork-point diffs: the current branch's
/// upstream if set, otherwise the first of `main`/`master` that exists.
pub fn default_fork_base() -> Result<String, Box<dyn Error>> {
    if let Some(upstream) = get_upstream_branch()? {
        return Ok(upstream);
    }
    for name in ["main", "master"] {
        let exists = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", name])
            .output()?
            .status
            .success();
        if exists {
            return Ok(name.to_string());
        }
    }
    Err("no upstream and no main/master found; specify a base: giff -b <ref>".into())
}

/// Best common ancestor of `base` and `head` (`git merge-base`).
fn merge_base(base: &str, head: &str) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["merge-base", base, head])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git merge-base {} {} failed: {}",
            base,
            head,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let fork = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if fork.is_empty() {
        return Err(format!("git merge-base {} {} produced no output", base, head).into());
    }
    Ok(fork)
}

/// Map parsed CLI inputs to a [`DiffSource`]. Precedence: custom diff args
/// override everything; `--branch` (with an optional explicit base from
/// `from`) beats positional refs; then two refs, one ref, or uncommitted.
pub fn select_diff_source(
    diff_args: Option<&str>,
    branch: bool,
    from: &str,
    to: &str,
) -> DiffSource {
    if let Some(args) = diff_args {
        DiffSource::CustomArgs(args.to_string())
    } else if branch {
        DiffSource::SinceFork {
            base: (!from.is_empty()).then(|| from.to_string()),
        }
    } else if !from.is_empty() && !to.is_empty() {
        DiffSource::Between {
            from: from.to_string(),
            to: to.to_string(),
        }
    } else if !from.is_empty() {
        DiffSource::ToRef(from.to_string())
    } else {
        DiffSource::Uncommitted
    }
}

// Compare two references (git diff <from>..<to>)
pub fn get_changes_between(
    from: &str,
    to: &str,
    context: Option<usize>,
) -> Result<DiffPayload, Box<dyn Error>> {
    let diff_output = get_diff_output_with_args(&[&format!("{}..{}", from, to)], context)?;
    let (files, meta) = parse_diff_output(&diff_output)?;
    Ok(DiffPayload {
        files,
        meta,
        left_label: from.to_string(),
        right_label: to.to_string(),
    })
}

/// Snapshot of the current branch's relationship to its upstream, used by
/// the UI status bar. `ahead`/`behind` are zero when there is no upstream or
/// the counts cannot be determined.
#[derive(Clone, Debug, Default)]
pub struct BranchStatus {
    pub name: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

pub fn branch_status() -> Result<BranchStatus, Box<dyn Error>> {
    let name = current_branch().unwrap_or_default();
    let upstream = get_upstream_branch().ok().flatten();

    let (ahead, behind) = match &upstream {
        Some(up) => count_ahead_behind(up).unwrap_or((0, 0)),
        None => (0, 0),
    };

    Ok(BranchStatus {
        name,
        upstream,
        ahead,
        behind,
    })
}

fn count_ahead_behind(upstream: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let output = Command::new("git")
        .args([
            "rev-list",
            "--left-right",
            "--count",
            &format!("{}...HEAD", upstream),
        ])
        .output()?;
    if !output.status.success() {
        return Ok((0, 0));
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let mut parts = s.split_whitespace();
    let behind = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    Ok((ahead, behind))
}

pub fn get_upstream_branch() -> Result<Option<String>, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD@{u}"])
        .output()?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Build the argv for a `git <subcmd> --no-color [--unified=N] <extra...>`
/// invocation. Pulled out for testability and to centralize the `--unified`
/// flag formatting across the diff and show paths.
fn build_git_args(subcmd: &str, extra: &[&str], context: Option<usize>) -> Vec<String> {
    let mut out = vec![subcmd.to_string(), "--no-color".to_string()];
    if let Some(n) = context {
        out.push(format!("--unified={}", n));
    }
    out.extend(extra.iter().map(|s| s.to_string()));
    out
}

fn get_diff_output_with_args(
    args: &[&str],
    context: Option<usize>,
) -> Result<String, Box<dyn Error>> {
    let cmd_args = build_git_args("diff", args, context);
    let output = Command::new("git").args(&cmd_args).output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to execute git diff command: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    Ok(String::from_utf8(output.stdout)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

fn extract_left_label(args: &str) -> String {
    args.split("..")
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Base".to_string())
}

fn extract_right_label(args: &str) -> String {
    args.split("..")
        .nth(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Target".to_string())
}

fn parse_diff_output(diff_output: &str) -> Result<(FileChanges, FileMetaMap), Box<dyn Error>> {
    let mut file_changes = HashMap::new();
    let mut file_meta: FileMetaMap = HashMap::new();
    let mut current_file = String::new();
    let mut base_lines = Vec::new();
    let mut head_lines = Vec::new();
    let mut base_line_number = 1;
    let mut head_line_number = 1;
    // Rename headers (`rename from`, `rename to`, `similarity index N%`) can
    // appear in any order within a file's header block. Buffer them until the
    // file's section ends, then assemble a `RenameInfo`.
    let mut pending_rename_from: Option<String> = None;
    let mut pending_similarity: Option<u8> = None;

    let flush_meta = |file: &str,
                      meta: &mut FileMetaMap,
                      rename_from: &mut Option<String>,
                      similarity: &mut Option<u8>| {
        if let Some(from) = rename_from.take() {
            // Default to 100% when git omits the similarity line (rare, but
            // technically allowed). A bare `rename from`/`to` with no
            // similarity header means "exact rename" in practice.
            let sim = similarity.take().unwrap_or(100);
            meta.entry(file.to_string()).or_default().rename = Some(RenameInfo {
                from,
                similarity: sim,
            });
        } else {
            *similarity = None;
        }
    };

    for line in diff_output.lines() {
        let trimmed = line.trim_end();

        // Skip the "no newline at end of file" marker — it is not
        // content and should not be rendered or counted.
        if trimmed.starts_with("\\ ") {
            continue;
        }

        let trimmed_line = ANSI_ESCAPE_RE.replace_all(trimmed, "");

        // Handle file header
        if let Some(caps) = DIFF_FILE_RE.captures(trimmed_line.as_ref()) {
            if !current_file.is_empty() {
                flush_meta(
                    &current_file,
                    &mut file_meta,
                    &mut pending_rename_from,
                    &mut pending_similarity,
                );
                file_changes.insert(
                    std::mem::take(&mut current_file),
                    (
                        std::mem::take(&mut base_lines),
                        std::mem::take(&mut head_lines),
                    ),
                );
            }

            // Use second capture group as file path in most cases (the "b/" file)
            current_file = match caps.get(2) {
                Some(m) => m.as_str().to_string(),
                None => continue,
            };
            base_line_number = 1;
            head_line_number = 1;
            continue;
        }

        // Handle hunk header
        if let Some(caps) = HUNK_HEADER_RE.captures(trimmed_line.as_ref()) {
            base_line_number = caps
                .get(1)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(1);
            head_line_number = caps
                .get(2)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(1);
            continue;
        }

        // Capture rename metadata before falling through to the generic skip.
        if let Some(rest) = trimmed_line.strip_prefix("rename from ") {
            pending_rename_from = Some(rest.to_string());
            continue;
        }
        if trimmed_line.starts_with("rename to ") {
            // The destination path is just `current_file` — no need to store it
            // separately. Recording the presence of the header is enough.
            continue;
        }
        if let Some(caps) = SIMILARITY_RE.captures(trimmed_line.as_ref()) {
            if let Some(n) = caps.get(1).and_then(|m| m.as_str().parse::<u8>().ok()) {
                pending_similarity = Some(n.min(100));
            }
            continue;
        }

        // Skip remaining metadata lines
        if trimmed_line.starts_with("index")
            || trimmed_line.starts_with("---")
            || trimmed_line.starts_with("+++")
            || trimmed_line.starts_with("@@")
            || trimmed_line.starts_with("new file mode")
            || trimmed_line.starts_with("new mode")
            || trimmed_line.starts_with("old mode")
            || trimmed_line.starts_with("deleted file mode")
            || trimmed_line.starts_with("copy from")
            || trimmed_line.starts_with("copy to")
            || trimmed_line.starts_with("dissimilarity index")
            || trimmed_line.starts_with("Binary files")
        {
            continue;
        }

        // Process diff lines
        if trimmed_line.starts_with('-') {
            base_lines.push((base_line_number, trimmed_line.to_string()));
            base_line_number += 1;
        } else if trimmed_line.starts_with('+') {
            head_lines.push((head_line_number, trimmed_line.to_string()));
            head_line_number += 1;
        } else {
            base_lines.push((base_line_number, trimmed_line.to_string()));
            head_lines.push((head_line_number, trimmed_line.to_string()));
            base_line_number += 1;
            head_line_number += 1;
        }
    }

    // Add last file changes
    if !current_file.is_empty() {
        flush_meta(
            &current_file,
            &mut file_meta,
            &mut pending_rename_from,
            &mut pending_similarity,
        );
        file_changes.insert(current_file, (base_lines, head_lines));
    }

    Ok((file_changes, file_meta))
}

#[derive(Clone)]
pub enum ChangeOp {
    /// Replace line at the given 1-indexed base position with new content
    Replace(usize, String),
    /// Delete line at the given 1-indexed base position
    Delete(usize),
    /// Insert content at the given 1-indexed base position.
    /// `order` is the original head line number, used to keep multiple
    /// insertions at the same base position in the correct order.
    Insert {
        base_pos: usize,
        order: usize,
        content: String,
    },
}

impl ChangeOp {
    fn line_num(&self) -> usize {
        match self {
            ChangeOp::Replace(n, _) | ChangeOp::Delete(n) => *n,
            ChangeOp::Insert { base_pos, .. } => *base_pos,
        }
    }
}

pub fn git_repo_root() -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        return Err("Not in a git repository".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Return the current branch name (e.g. "main"). Errors when HEAD is
/// detached or git is otherwise unavailable.
pub fn current_branch() -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Failed to resolve current branch: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Return all configured remote names, in the order `git remote` reports.
pub fn list_remotes() -> Result<Vec<String>, Box<dyn Error>> {
    let output = Command::new("git").args(["remote"]).output()?;
    if !output.status.success() {
        return Err(format!(
            "Failed to list remotes: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

pub fn has_uncommitted_changes() -> Result<bool, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()?;
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Join a repo-relative diff path onto `repo_root`, rejecting anything that
/// would escape the repo.
///
/// `git diff` only ever emits repo-relative paths with forward slashes and no
/// `..` segments, so any input that fails these checks is either a malformed
/// or hostile diff and must not be written to.
fn resolve_within_root(repo_root: &Path, file_path: &str) -> Result<PathBuf, Box<dyn Error>> {
    use std::path::Component;

    if file_path.is_empty() {
        return Err("empty file path in diff".into());
    }

    for component in Path::new(file_path).components() {
        match component {
            Component::ParentDir => {
                return Err(format!("path traversal rejected: {}", file_path).into());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("absolute path not allowed in diff: {}", file_path).into());
            }
            _ => {}
        }
    }

    Ok(repo_root.join(file_path))
}

/// Resolve a diff file path (relative to repo root) to an absolute path.
fn resolve_diff_path(file_path: &str) -> Result<PathBuf, Box<dyn Error>> {
    let repo_root = git_repo_root()?;
    resolve_within_root(Path::new(&repo_root), file_path)
}

/// Apply change operations to file content lines and return the result.
/// This is the core logic extracted for testability.
pub fn apply_operations(lines: &[String], operations: &[ChangeOp]) -> Vec<String> {
    let mut lines: Vec<String> = lines.to_vec();

    // Phase 1: Apply Delete and Replace operations (already in base coordinates).
    // Process in descending line-number order so that removals at higher
    // positions don't shift indices for lower positions.
    let mut base_ops: Vec<&ChangeOp> = operations
        .iter()
        .filter(|op| matches!(op, ChangeOp::Replace(..) | ChangeOp::Delete(..)))
        .collect();
    base_ops.sort_by_key(|op| std::cmp::Reverse(op.line_num()));

    let mut deleted_positions: Vec<usize> = Vec::new();

    for op in &base_ops {
        match op {
            ChangeOp::Replace(line_num, content) => {
                if *line_num == 0 {
                    continue;
                }
                let idx = line_num - 1;
                if idx < lines.len() {
                    lines[idx] = content.clone();
                }
            }
            ChangeOp::Delete(line_num) => {
                if *line_num == 0 {
                    continue;
                }
                let idx = line_num - 1;
                if idx < lines.len() {
                    lines.remove(idx);
                    deleted_positions.push(*line_num);
                }
            }
            _ => {}
        }
    }

    // Phase 2: Apply Insert operations, adjusting positions for prior deletions.
    // Sort by (base_pos DESC, order DESC) so that multiple inserts at the
    // same base position end up in the correct source order: the last one
    // processed at a position pushes earlier ones down.
    let mut insert_ops: Vec<&ChangeOp> = operations
        .iter()
        .filter(|op| matches!(op, ChangeOp::Insert { .. }))
        .collect();
    insert_ops.sort_by(|a, b| {
        let pos_cmp = b.line_num().cmp(&a.line_num());
        if pos_cmp != std::cmp::Ordering::Equal {
            return pos_cmp;
        }
        // Tiebreak: higher order (head line number) processed first
        let a_order = if let ChangeOp::Insert { order, .. } = a {
            *order
        } else {
            0
        };
        let b_order = if let ChangeOp::Insert { order, .. } = b {
            *order
        } else {
            0
        };
        b_order.cmp(&a_order)
    });

    // Sort so we can binary-search instead of scanning the whole
    // list for every insert (avoids O(n²) on large diffs).
    deleted_positions.sort_unstable();

    for op in &insert_ops {
        if let ChangeOp::Insert {
            base_pos, content, ..
        } = op
        {
            if *base_pos == 0 {
                continue;
            }
            // Adjust for lines that were deleted at positions before this one
            let deletes_before = deleted_positions.partition_point(|&d| d < *base_pos);
            let adjusted = base_pos.saturating_sub(deletes_before);
            let idx = adjusted.saturating_sub(1).min(lines.len());
            lines.insert(idx, content.clone());
        }
    }

    lines
}

pub fn apply_changes(file_path: &str, operations: &[ChangeOp]) -> Result<(), Box<dyn Error>> {
    if operations.is_empty() {
        return Ok(());
    }

    let full_path = resolve_diff_path(file_path)?;
    let original_content = std::fs::read_to_string(&full_path)?;
    let has_trailing_newline = original_content.ends_with('\n');
    let lines: Vec<String> = original_content.lines().map(|s| s.to_string()).collect();

    let result_lines = apply_operations(&lines, operations);

    let mut result = result_lines.join("\n");
    if has_trailing_newline {
        result.push('\n');
    }
    std::fs::write(&full_path, result)?;

    Ok(())
}

pub fn check_rebase_needed() -> Result<Option<String>, Box<dyn Error>> {
    // Check if we're in a git repository
    let status = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()?;

    if !status.status.success() {
        return Ok(None);
    }

    // Get current branch name
    let current_branch = match current_branch() {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };

    // Check if branch has an upstream and get its name
    let upstream_output = match Command::new("git")
        .args([
            "rev-parse",
            "--abbrev-ref",
            &format!("{}@{{u}}", current_branch),
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Ok(None), // No upstream configured
    };

    let upstream_name = String::from_utf8_lossy(&upstream_output.stdout)
        .trim()
        .to_string();

    // Check branch status relative to upstream
    let status_output = Command::new("git").args(["status", "-sb"]).output()?;
    let status_text = String::from_utf8_lossy(&status_output.stdout).to_string();

    // Check for diverged state (both ahead and behind)
    if status_text.contains("ahead") && status_text.contains("behind") {
        return Ok(Some(format!(
            "Your branch '{}' has diverged from '{}'.\nConsider rebasing to integrate changes cleanly.",
            current_branch, upstream_name
        )));
    }

    // Check for behind-only state
    if status_text.contains("[behind") {
        return Ok(Some(format!(
            "Your branch '{}' is behind '{}'. A rebase is recommended.",
            current_branch, upstream_name
        )));
    }

    Ok(None)
}

pub fn perform_rebase(upstream: &str) -> Result<bool, Box<dyn Error>> {
    if has_uncommitted_changes()? {
        return Err(
            "Cannot rebase: you have uncommitted changes. Please commit or stash them first."
                .into(),
        );
    }

    let output = Command::new("git").args(["rebase", upstream]).output()?;

    if !output.status.success() {
        // Abort the failed rebase to leave the repo in a clean state
        let _ = Command::new("git").args(["rebase", "--abort"]).output();
        return Ok(false);
    }

    Ok(true)
}

/// Run `git pull --rebase`. On failure, defensively run `git rebase --abort`
/// so the repo is left in a clean state, then return the captured stderr.
pub fn pull_rebase() -> Result<(), Box<dyn Error>> {
    let output = Command::new("git").args(["pull", "--rebase"]).output()?;
    if !output.status.success() {
        let _ = Command::new("git").args(["rebase", "--abort"]).output();
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

/// Run `git push` for the current branch's configured upstream.
pub fn push() -> Result<(), Box<dyn Error>> {
    let output = Command::new("git").args(["push"]).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

/// Gather everything the commit-message generator needs in one string:
/// the last few commit subjects (for style matching) followed by the full
/// unified diff of every change that would land in the next commit
/// (staged + unstaged on tracked files, plus a synthesized diff for each
/// untracked file).
pub fn get_commit_context() -> Result<String, Box<dyn Error>> {
    let mut out = String::new();

    // Recent commit subjects so the model can match prevailing style/language.
    let log = Command::new("git")
        .args(["log", "-10", "--pretty=format:%s", "--no-color"])
        .output()?;
    if log.status.success() {
        let subjects = String::from_utf8_lossy(&log.stdout);
        if !subjects.trim().is_empty() {
            out.push_str("=== Recent commits (for style reference) ===\n");
            out.push_str(subjects.trim_end());
            out.push_str("\n\n");
        }
    }

    out.push_str("=== Diff to be committed ===\n");

    // Staged + unstaged combined against HEAD (covers tracked changes).
    let diff_out = Command::new("git")
        .args(["diff", "HEAD", "--no-color"])
        .output()?;
    if diff_out.status.success() {
        out.push_str(&String::from_utf8_lossy(&diff_out.stdout));
    }

    // Synthesize a diff for each untracked file so the model sees new files too.
    if let Ok(repo_root) = git_repo_root() {
        for file in list_untracked_files(&repo_root) {
            if let Some(extra) = diff_untracked_file(&repo_root, &file) {
                out.push_str(&extra);
            }
        }
    }

    Ok(out)
}

/// Stage every change in the working tree (`git add -A`).
pub fn stage_all() -> Result<(), Box<dyn Error>> {
    let output = Command::new("git").args(["add", "-A"]).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

/// Create a commit with the given message. Multi-line messages are passed
/// through `git commit -m <msg>` verbatim, which git handles correctly.
pub fn commit_with_message(message: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

/// Run `git push -u <remote> <branch>` to publish a branch and set its upstream.
pub fn push_set_upstream(remote: &str, branch: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(["push", "-u", remote, branch])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_left_label / extract_right_label ────────────────────────

    #[test]
    fn left_label_from_range() {
        assert_eq!(extract_left_label("main..feature"), "main");
    }

    #[test]
    fn right_label_from_range() {
        assert_eq!(extract_right_label("main..feature"), "feature");
    }

    #[test]
    fn left_label_no_dots_returns_input() {
        // No ".." means the whole string is the left side
        assert_eq!(extract_left_label("--cached"), "--cached");
    }

    #[test]
    fn right_label_no_dots_returns_default() {
        // No ".." means there is no right side
        assert_eq!(extract_right_label("--cached"), "Target");
    }

    #[test]
    fn labels_with_empty_sides() {
        // "..feature" → left side is empty → default
        assert_eq!(extract_left_label("..feature"), "Base");
        // "main.." → right side is empty → default
        assert_eq!(extract_right_label("main.."), "Target");
    }

    #[test]
    fn labels_trim_whitespace() {
        assert_eq!(extract_left_label(" main .. feature "), "main");
        assert_eq!(extract_right_label(" main .. feature "), "feature");
    }

    // ── parse_diff_output: metadata skipping ────────────────────────────

    #[test]
    fn parse_skips_rename_metadata() {
        let diff = "\
diff --git a/old.rs b/new.rs
similarity index 95%
rename from old.rs
rename to new.rs
index abc..def 100644
--- a/old.rs
+++ b/new.rs
@@ -1,3 +1,3 @@
 fn main() {
-    old();
+    new();
 }
";
        let (changes, meta) = parse_diff_output(diff).unwrap();
        let (base, head) = changes.get("new.rs").expect("file should be present");
        let rename = meta
            .get("new.rs")
            .and_then(|m| m.rename.as_ref())
            .expect("rename metadata should be captured");
        assert_eq!(rename.from, "old.rs");
        assert_eq!(rename.similarity, 95);
        assert!(!meta["new.rs"].is_pure_rename());
        assert!(meta["new.rs"].is_rename());
        // Only actual diff lines should be present, no metadata leaking as context
        assert!(
            !base
                .iter()
                .any(|(_, l)| l.contains("similarity") || l.contains("rename")),
            "metadata leaked into base lines: {:?}",
            base
        );
        assert!(
            !head
                .iter()
                .any(|(_, l)| l.contains("similarity") || l.contains("rename")),
            "metadata leaked into head lines: {:?}",
            head
        );
    }

    #[test]
    fn parse_captures_pure_rename() {
        // A pure rename (100% similarity) has no hunks — git emits only the
        // header block. The file should still show up in the map so the UI
        // can list it (and optionally hide it via the rename filter).
        let diff = "\
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
";
        let (changes, meta) = parse_diff_output(diff).unwrap();
        let (base, head) = changes.get("new.txt").expect("file should be present");
        assert!(
            base.is_empty() && head.is_empty(),
            "pure rename has no hunks"
        );
        let m = meta.get("new.txt").expect("meta should be present");
        assert!(m.is_pure_rename());
        assert_eq!(m.rename.as_ref().unwrap().from, "old.txt");
        assert_eq!(m.rename.as_ref().unwrap().similarity, 100);
    }

    #[test]
    fn parse_non_rename_has_no_rename_meta() {
        let diff = "\
diff --git a/a.rs b/a.rs
index abc..def 100644
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-x
+y
";
        let (_changes, meta) = parse_diff_output(diff).unwrap();
        // Either no entry, or an entry with no rename info — both acceptable.
        if let Some(m) = meta.get("a.rs") {
            assert!(m.rename.is_none());
            assert!(!m.is_pure_rename());
        }
    }

    #[test]
    fn parse_skips_no_newline_marker() {
        let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,2 +1,2 @@
-old line
\\ No newline at end of file
+new line
\\ No newline at end of file
";
        let (changes, _meta) = parse_diff_output(diff).unwrap();
        let (base, head) = changes.get("file.rs").expect("file should be present");
        assert!(
            !base.iter().any(|(_, l)| l.contains("No newline")),
            "no-newline marker leaked into base lines: {:?}",
            base
        );
        assert!(
            !head.iter().any(|(_, l)| l.contains("No newline")),
            "no-newline marker leaked into head lines: {:?}",
            head
        );
    }

    #[test]
    fn parse_skips_binary_files_line() {
        let diff = "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
";
        let (changes, _meta) = parse_diff_output(diff).unwrap();
        // Binary file should have entry but no content lines
        if let Some((base, head)) = changes.get("image.png") {
            assert!(base.is_empty());
            assert!(head.is_empty());
        }
        // Or no entry at all — both are acceptable
    }

    // ── build_git_args ──────────────────────────────────────────────────

    #[test]
    fn diff_args_without_context_omits_unified_flag() {
        let args = build_git_args("diff", &[], None);
        assert_eq!(args, vec!["diff", "--no-color"]);
    }

    #[test]
    fn diff_args_passes_extra_args_through() {
        let args = build_git_args("diff", &["HEAD~1..HEAD"], None);
        assert_eq!(args, vec!["diff", "--no-color", "HEAD~1..HEAD"]);
    }

    #[test]
    fn diff_args_with_context_inserts_unified_flag() {
        let args = build_git_args("diff", &[], Some(42));
        assert_eq!(args, vec!["diff", "--no-color", "--unified=42"]);
    }

    #[test]
    fn diff_args_with_context_and_extras() {
        let args = build_git_args("diff", &["HEAD"], Some(99999));
        assert_eq!(args, vec!["diff", "--no-color", "--unified=99999", "HEAD"]);
    }

    #[test]
    fn show_args_with_context_inserts_unified_before_extras() {
        let args = build_git_args(
            "show",
            &["--format=", "-m", "--first-parent", "abc123"],
            Some(50),
        );
        assert_eq!(
            args,
            vec![
                "show",
                "--no-color",
                "--unified=50",
                "--format=",
                "-m",
                "--first-parent",
                "abc123",
            ]
        );
    }

    // ── resolve_within_root: path-traversal containment ─────────────────

    #[test]
    fn resolve_within_root_accepts_repo_relative_path() {
        let root = Path::new("/repo");
        let resolved = resolve_within_root(root, "src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/repo/src/main.rs"));
    }

    #[test]
    fn resolve_within_root_accepts_nested_path() {
        let root = Path::new("/repo");
        let resolved = resolve_within_root(root, "a/b/c/file.txt").unwrap();
        assert_eq!(resolved, PathBuf::from("/repo/a/b/c/file.txt"));
    }

    #[test]
    fn resolve_within_root_rejects_parent_traversal() {
        let root = Path::new("/repo");
        let err = resolve_within_root(root, "../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn resolve_within_root_rejects_embedded_parent_traversal() {
        let root = Path::new("/repo");
        let err = resolve_within_root(root, "src/../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn resolve_within_root_rejects_absolute_unix_path() {
        let root = Path::new("/repo");
        let err = resolve_within_root(root, "/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("absolute path"));
    }

    #[test]
    fn resolve_within_root_rejects_empty_path() {
        let root = Path::new("/repo");
        let err = resolve_within_root(root, "").unwrap_err();
        assert!(err.to_string().contains("empty file path"));
    }

    // ── select_diff_source: CLI → DiffSource precedence ─────────────────

    #[test]
    fn select_branch_no_base_is_sincefork_none() {
        assert_eq!(
            select_diff_source(None, true, "", ""),
            DiffSource::SinceFork { base: None }
        );
    }

    #[test]
    fn select_branch_with_base_is_sincefork_some() {
        assert_eq!(
            select_diff_source(None, true, "main", ""),
            DiffSource::SinceFork {
                base: Some("main".to_string())
            }
        );
    }

    #[test]
    fn select_branch_ignores_second_positional() {
        // `giff -b main feature` uses `main` as the base; `to` is dropped.
        assert_eq!(
            select_diff_source(None, true, "main", "feature"),
            DiffSource::SinceFork {
                base: Some("main".to_string())
            }
        );
    }

    #[test]
    fn select_one_ref_is_toref() {
        assert_eq!(
            select_diff_source(None, false, "main", ""),
            DiffSource::ToRef("main".to_string())
        );
    }

    #[test]
    fn select_two_refs_is_between() {
        assert_eq!(
            select_diff_source(None, false, "a", "b"),
            DiffSource::Between {
                from: "a".to_string(),
                to: "b".to_string()
            }
        );
    }

    #[test]
    fn select_no_args_is_uncommitted() {
        assert_eq!(
            select_diff_source(None, false, "", ""),
            DiffSource::Uncommitted
        );
    }

    #[test]
    fn select_diff_args_wins_over_branch() {
        assert_eq!(
            select_diff_source(Some("--stat"), true, "x", "y"),
            DiffSource::CustomArgs("--stat".to_string())
        );
    }
}
