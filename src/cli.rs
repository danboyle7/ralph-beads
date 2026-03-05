use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "ralph", about = "Ralph Wiggum in Rust")]
pub(crate) struct Cli {
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[arg(long)]
    pub(crate) once: bool,
    #[arg(long, value_name = "N")]
    pub(crate) iterations: Option<usize>,
    #[arg(short, long)]
    pub(crate) verbose: bool,
    #[arg(long)]
    pub(crate) init: bool,
    #[arg(long)]
    pub(crate) summary: bool,
    #[arg(long)]
    pub(crate) plain: bool,
    #[arg(long)]
    pub(crate) debug: bool,
}

#[derive(Clone)]
pub(crate) struct Paths {
    pub(crate) project_dir: PathBuf,
    pub(crate) ralph_dir: PathBuf,
    pub(crate) prompt_file: PathBuf,
    pub(crate) progress_file: PathBuf,
    pub(crate) logs_dir: PathBuf,
    pub(crate) archive_dir: PathBuf,
    pub(crate) last_run_file: PathBuf,
}

impl Paths {
    pub(crate) fn from_cwd() -> Result<Self> {
        let project_dir = std::env::current_dir().context("failed to get current directory")?;
        let ralph_dir = project_dir.join(".ralph");
        Ok(Self {
            project_dir,
            prompt_file: ralph_dir.join("prompt.md"),
            progress_file: ralph_dir.join("progress.txt"),
            logs_dir: ralph_dir.join("logs"),
            archive_dir: ralph_dir.join("archive"),
            last_run_file: ralph_dir.join(".last-run"),
            ralph_dir,
        })
    }
}
