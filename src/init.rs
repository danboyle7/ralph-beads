use std::fs;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::Paths;

pub(crate) fn init_project(paths: &Paths, default_prompt: &str) -> Result<()> {
    if paths.ralph_dir.exists() {
        bail!(".ralph already exists in {}", paths.project_dir.display());
    }

    ensure_beads_initialized(paths)?;

    fs::create_dir_all(&paths.archive_dir).context("failed to create .ralph/archive")?;
    fs::create_dir_all(&paths.logs_dir).context("failed to create .ralph/logs")?;
    fs::write(&paths.prompt_file, default_prompt).context("failed to write default prompt")?;

    println!("Created {}", paths.ralph_dir.display());
    println!("Edit {}", paths.prompt_file.display());
    Ok(())
}

fn ensure_beads_initialized(paths: &Paths) -> Result<()> {
    if paths.project_dir.join(".beads").exists() {
        return Ok(());
    }

    which::which("bd").context("bd not found in PATH (required to initialize beads)")?;
    let output = Command::new("bd")
        .arg("init")
        .current_dir(&paths.project_dir)
        .output()
        .context("failed to run `bd init`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "`bd init` failed in {}.\nstdout:\n{}\nstderr:\n{}",
            paths.project_dir.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    println!("Initialized beads in {}", paths.project_dir.display());
    Ok(())
}
