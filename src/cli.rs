use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum CliCommand {
    Init,
    Doctor,
    Preflight,
    UpgradePrompts,
    Summary,
    Cleanup,
    Reflect,
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "ralph",
    about = "Ralph Wiggum in Rust",
    long_version = crate::build_info::LONG_VERSION
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<CliCommand>,
    #[arg(long, global = true)]
    pub(crate) dry_run: bool,
    #[arg(long, global = true)]
    pub(crate) once: bool,
    #[arg(long, value_name = "N", global = true)]
    pub(crate) iterations: Option<usize>,
    #[arg(short, long, global = true)]
    pub(crate) verbose: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) init: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) doctor: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) cleanup: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) reflect: bool,
    #[arg(long, value_name = "N", global = true)]
    pub(crate) reflect_every: Option<usize>,
    #[arg(long, hide = true, global = true)]
    pub(crate) summary: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) preflight: bool,
    #[arg(long, hide = true, global = true)]
    pub(crate) upgrade_prompts: bool,
    #[arg(long, global = true)]
    pub(crate) plain: bool,
    #[arg(long, global = true)]
    pub(crate) debug: bool,
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[arg(long, global = true)]
    pub(crate) snapshot_consistency: bool,
    #[arg(long, global = true)]
    pub(crate) skip_snapshot_consistency: bool,
}

#[derive(Clone)]
pub(crate) struct Paths {
    pub(crate) project_dir: PathBuf,
    pub(crate) ralph_dir: PathBuf,
    pub(crate) prompts_dir: PathBuf,
    pub(crate) meta_prompt_file: PathBuf,
    pub(crate) issue_prompt_file: PathBuf,
    pub(crate) cleanup_prompt_file: PathBuf,
    pub(crate) quality_check_prompt_file: PathBuf,
    pub(crate) code_review_check_prompt_file: PathBuf,
    pub(crate) validation_check_prompt_file: PathBuf,
    pub(crate) progress_file: PathBuf,
    pub(crate) logs_dir: PathBuf,
    pub(crate) archive_dir: PathBuf,
    pub(crate) last_run_file: PathBuf,
    pub(crate) lock_file: PathBuf,
    pub(crate) state_file: PathBuf,
    pub(crate) issue_snapshot_file: PathBuf,
    pub(crate) config_file: PathBuf,
    pub(crate) template_version_file: PathBuf,
    pub(crate) agents_file: PathBuf,
    pub(crate) rules_file: PathBuf,
}

impl Paths {
    pub(crate) fn from_cwd() -> Result<Self> {
        let project_dir = std::env::current_dir().context("failed to get current directory")?;
        let ralph_dir = project_dir.join(".ralph");
        let prompts_dir = ralph_dir.join("prompts");
        Ok(Self {
            project_dir: project_dir.clone(),
            meta_prompt_file: prompts_dir.join("ralph.md"),
            issue_prompt_file: prompts_dir.join("issue.md"),
            cleanup_prompt_file: prompts_dir.join("cleanup.md"),
            quality_check_prompt_file: prompts_dir.join("quality-check.md"),
            code_review_check_prompt_file: prompts_dir.join("code-review-check.md"),
            validation_check_prompt_file: prompts_dir.join("validation-check.md"),
            progress_file: ralph_dir.join("progress.txt"),
            logs_dir: ralph_dir.join("logs"),
            archive_dir: ralph_dir.join("archive"),
            last_run_file: ralph_dir.join(".last-run"),
            lock_file: ralph_dir.join("run.lock"),
            state_file: ralph_dir.join("state.json"),
            issue_snapshot_file: ralph_dir.join("issue-snapshot.json"),
            config_file: ralph_dir.join("config.toml"),
            template_version_file: prompts_dir.join(".template-version"),
            agents_file: project_dir.join("AGENTS.md"),
            rules_file: project_dir.join("rules.md"),
            ralph_dir,
            prompts_dir,
        })
    }
}
