use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};

const SYSTEM_PROMPT: &str = r#"You generate git commit messages in Conventional Commits format.

The user will paste recent commit subjects (for style reference) and the unified diff to be committed.
Output ONLY the commit message text — no preamble, no markdown code fences, no quotes, no explanation.

Format:
<type>(<scope>): <subject>

[optional body]

Rules:
- Type: feat | fix | docs | style | refactor | perf | test | build | ci | chore | revert
- Scope: derive from the dominant changed path (e.g. `feat(auth):`, `fix(api):`). Omit scope when changes span many unrelated areas.
- Subject: imperative mood ("add" not "added"), lowercase first letter, no trailing period, <= 100 chars.
- Body: OPTIONAL. Wrap at ~72 chars. Explain WHY/HOW, not WHAT. Skip the body entirely for trivial changes (typos, dep bumps, renames).
- Language: match the language used in the recent commits provided. Default to English.
- Breaking change footer: `BREAKING CHANGE: <description>`

Output ONLY the commit message text."#;

/// Generate a commit message by invoking the local `claude` CLI in print mode.
/// `context` is sent to claude via stdin; it should include recent commit
/// subjects and the unified diff to be summarized.
pub fn generate_commit_message(context: &str) -> Result<String, Box<dyn Error>> {
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--model",
            "haiku",
            "--system-prompt",
            SYSTEM_PROMPT,
            "--output-format",
            "text",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn `claude` CLI: {}. Is it installed and on PATH?",
                e
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(context.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    let stdout = String::from_utf8(output.stdout)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        // Some failure paths (e.g. "Not logged in") print to stdout; surface
        // whichever stream actually carries the explanation.
        let detail = if !stderr.is_empty() {
            stderr
        } else {
            stdout.trim().to_string()
        };
        return Err(format!("claude exited with error: {}", detail).into());
    }

    let cleaned = clean_message(&stdout);
    if cleaned.is_empty() {
        return Err("claude returned an empty commit message".into());
    }
    Ok(cleaned)
}

/// Strip markdown code fences and surrounding whitespace from `s` so the
/// message is ready to hand straight to `git commit -m`.
fn clean_message(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with("```") {
        // Drop opening fence line (e.g. ```text) and any trailing ```.
        let after_open = trimmed.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        let body = after_open
            .rsplit_once("```")
            .map(|(before, _)| before)
            .unwrap_or(after_open);
        return body.trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_message_passes_through_plain_text() {
        let msg = "feat(ui): add commit shortcut\n\nBody here.";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_trims_surrounding_whitespace() {
        assert_eq!(clean_message("  \n\nfeat: x\n\n  "), "feat: x");
    }

    #[test]
    fn clean_message_strips_code_fences() {
        let fenced = "```\nfeat: x\n\nBody.\n```";
        assert_eq!(clean_message(fenced), "feat: x\n\nBody.");
    }

    #[test]
    fn clean_message_strips_labelled_code_fences() {
        let fenced = "```text\nfeat: x\n```";
        assert_eq!(clean_message(fenced), "feat: x");
    }
}
