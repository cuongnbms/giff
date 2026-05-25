mod commit;
mod config;
mod diff;
mod ui;

use clap::Parser;
use std::error::Error;

#[derive(Parser)]
#[command(author="bahdotsh", version, about, long_about = None)]
struct Args {
    /// Base reference for diff (commit, branch, etc.)
    #[arg(default_value = "")]
    from: String,

    /// Target reference for diff (commit, branch, etc.; defaults to current state)
    #[arg(default_value = "")]
    to: String,

    /// Pass this to run diff with custom arguments
    #[arg(short, long)]
    diff_args: Option<String>,

    #[arg(short, long, help = "Auto-rebase if needed")]
    auto_rebase: bool,

    /// Color theme ("dark", "light", or a custom theme name)
    #[arg(short, long)]
    theme: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    // Check if rebase is needed before proceeding
    if args.auto_rebase {
        if let Some(rebase_msg) = diff::check_rebase_needed()? {
            eprintln!("{}", rebase_msg);

            if let Some(upstream) = diff::get_upstream_branch()? {
                eprintln!("Auto-rebasing onto {}...", upstream);
                if diff::perform_rebase(&upstream)? {
                    eprintln!("Rebase successful!");
                } else {
                    eprintln!("Rebase failed. There might be conflicts to resolve.");
                    return Err("Rebase failed".into());
                }
            }
        }
    }

    // Pick the diff source based on arguments and fetch the initial diff.
    let diff_source = if let Some(diff_args) = &args.diff_args {
        diff::DiffSource::CustomArgs(diff_args.clone())
    } else if !args.from.is_empty() && !args.to.is_empty() {
        diff::DiffSource::Between {
            from: args.from.clone(),
            to: args.to.clone(),
        }
    } else if !args.from.is_empty() {
        diff::DiffSource::ToRef(args.from.clone())
    } else {
        diff::DiffSource::Uncommitted
    };

    let diff::DiffPayload {
        files: file_changes,
        meta: file_meta,
        left_label,
        right_label,
    } = diff_source.fetch()?;

    // Check if rebase is needed (once, reuse in UI)
    let rebase_notification = diff::check_rebase_needed()?;

    // Load config and resolve theme + initial UI defaults.
    let cfg = config::load_config();
    let theme = config::resolve_theme(&cfg, args.theme.as_deref());
    let defaults = ui::UiDefaults {
        view_mode: config::resolve_view_mode(&cfg),
        wrap_mode: config::resolve_wrap(&cfg),
    };
    if !ui::is_valid_syntax_theme(&theme.syntax_theme) {
        eprintln!(
            "Warning: syntax theme '{}' not found, using fallback",
            theme.syntax_theme
        );
    }

    // Start the interactive UI
    ui::run_app(
        file_changes,
        file_meta,
        left_label,
        right_label,
        theme,
        rebase_notification,
        diff_source,
        defaults,
    )?;

    Ok(())
}
