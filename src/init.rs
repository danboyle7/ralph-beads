use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::Paths;

pub(crate) struct PromptTemplates<'a> {
    pub(crate) meta: &'a str,
    pub(crate) issue: &'a str,
    pub(crate) cleanup: &'a str,
    pub(crate) quality_check: &'a str,
    pub(crate) code_review_check: &'a str,
    pub(crate) validation_check: &'a str,
}

pub(crate) fn init_project(paths: &Paths, templates: &PromptTemplates<'_>) -> Result<()> {
    if paths.ralph_dir.exists() {
        bail!(".ralph already exists in {}", paths.project_dir.display());
    }

    ensure_beads_initialized(paths)?;

    ensure_layout(paths)?;
    fs::write(&paths.meta_prompt_file, templates.meta)
        .context("failed to write default meta prompt")?;
    fs::write(&paths.issue_prompt_file, templates.issue)
        .context("failed to write default issue prompt")?;
    fs::write(&paths.cleanup_prompt_file, templates.cleanup)
        .context("failed to write default cleanup prompt")?;
    fs::write(&paths.quality_check_prompt_file, templates.quality_check)
        .context("failed to write default quality-check prompt")?;
    fs::write(
        &paths.code_review_check_prompt_file,
        templates.code_review_check,
    )
    .context("failed to write default code-review-check prompt")?;
    fs::write(
        &paths.validation_check_prompt_file,
        templates.validation_check,
    )
    .context("failed to write default validation-check prompt")?;

    println!("Created {}", paths.ralph_dir.display());
    println!("Edit {}", paths.issue_prompt_file.display());
    Ok(())
}

pub(crate) fn doctor_project(paths: &Paths, templates: &PromptTemplates<'_>) -> Result<()> {
    ensure_beads_initialized(paths)?;

    let mut changes = Vec::new();

    ensure_dir(&paths.ralph_dir, &mut changes)?;
    ensure_dir(&paths.archive_dir, &mut changes)?;
    ensure_dir(&paths.logs_dir, &mut changes)?;
    ensure_dir(&paths.prompts_dir, &mut changes)?;

    ensure_file(&paths.meta_prompt_file, templates.meta, &mut changes)?;

    if !paths.issue_prompt_file.exists() {
        if paths.legacy_issue_prompt_file.exists() {
            fs::copy(&paths.legacy_issue_prompt_file, &paths.issue_prompt_file).with_context(
                || {
                    format!(
                        "failed to migrate {} to {}",
                        paths.legacy_issue_prompt_file.display(),
                        paths.issue_prompt_file.display()
                    )
                },
            )?;
            changes.push(format!(
                "migrated {} -> {}",
                paths.legacy_issue_prompt_file.display(),
                paths.issue_prompt_file.display()
            ));
        } else if paths.legacy_root_prompt_file.exists() {
            fs::copy(&paths.legacy_root_prompt_file, &paths.issue_prompt_file).with_context(
                || {
                    format!(
                        "failed to migrate {} to {}",
                        paths.legacy_root_prompt_file.display(),
                        paths.issue_prompt_file.display()
                    )
                },
            )?;
            changes.push(format!(
                "migrated {} -> {}",
                paths.legacy_root_prompt_file.display(),
                paths.issue_prompt_file.display()
            ));
        } else {
            fs::write(&paths.issue_prompt_file, templates.issue)
                .context("failed to restore missing issue.md")?;
            changes.push(format!("restored {}", paths.issue_prompt_file.display()));
        }
    }

    ensure_file(&paths.cleanup_prompt_file, templates.cleanup, &mut changes)?;
    ensure_file(
        &paths.quality_check_prompt_file,
        templates.quality_check,
        &mut changes,
    )?;
    ensure_file(
        &paths.code_review_check_prompt_file,
        templates.code_review_check,
        &mut changes,
    )?;
    ensure_file(
        &paths.validation_check_prompt_file,
        templates.validation_check,
        &mut changes,
    )?;

    if changes.is_empty() {
        println!("Doctor found no missing Ralph files.");
    } else {
        println!("Doctor applied {} fix(es):", changes.len());
        for change in &changes {
            println!("- {change}");
        }
    }

    Ok(())
}

fn ensure_layout(paths: &Paths) -> Result<()> {
    fs::create_dir_all(&paths.archive_dir).context("failed to create .ralph/archive")?;
    fs::create_dir_all(&paths.logs_dir).context("failed to create .ralph/logs")?;
    fs::create_dir_all(&paths.prompts_dir).context("failed to create .ralph/prompts")?;
    Ok(())
}

fn ensure_dir(path: &Path, changes: &mut Vec<String>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    changes.push(format!("created {}", path.display()));
    Ok(())
}

fn ensure_file(path: &Path, content: &str, changes: &mut Vec<String>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    fs::write(path, content).with_context(|| format!("failed to create {}", path.display()))?;
    changes.push(format!("restored {}", path.display()));
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
