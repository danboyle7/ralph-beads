use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::cli::Paths;

pub(crate) const TEMPLATE_VERSION: &str = "2026-03-09.2";

pub(crate) struct PromptTemplates<'a> {
    pub(crate) meta: &'a str,
    pub(crate) issue: &'a str,
    pub(crate) cleanup: &'a str,
    pub(crate) repair: &'a str,
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
    fs::write(&paths.repair_prompt_file, templates.repair)
        .context("failed to write default repair prompt")?;
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
    fs::write(&paths.template_version_file, TEMPLATE_VERSION)
        .context("failed to write prompt template version file")?;
    fs::write(&paths.config_file, default_config_contents())
        .context("failed to write default Ralph config")?;
    if !paths.agents_file.exists() {
        fs::write(&paths.agents_file, default_agents_shell_contents())
            .context("failed to write AGENTS.md shell")?;
    }

    println!("Created {}", paths.ralph_dir.display());
    println!("Edit {}", paths.issue_prompt_file.display());
    if paths.agents_file.exists() {
        println!("Agent guidance: {}", paths.agents_file.display());
    }
    Ok(())
}

pub(crate) fn doctor_project(paths: &Paths, templates: &PromptTemplates<'_>) -> Result<()> {
    ensure_beads_initialized(paths)?;

    let mut changes = Vec::new();
    let mut notices = Vec::new();

    ensure_dir(&paths.ralph_dir, &mut changes)?;
    ensure_dir(&paths.archive_dir, &mut changes)?;
    ensure_dir(&paths.prompts_dir, &mut changes)?;

    ensure_file(&paths.meta_prompt_file, templates.meta, &mut changes)?;
    ensure_file(&paths.issue_prompt_file, templates.issue, &mut changes)?;

    ensure_file(&paths.cleanup_prompt_file, templates.cleanup, &mut changes)?;
    ensure_file(&paths.repair_prompt_file, templates.repair, &mut changes)?;
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
    ensure_file(&paths.template_version_file, TEMPLATE_VERSION, &mut changes)?;
    ensure_file(&paths.config_file, default_config_contents(), &mut changes)?;
    ensure_file(
        &paths.agents_file,
        default_agents_shell_contents(),
        &mut changes,
    )?;

    if let Ok(current_version) = fs::read_to_string(&paths.template_version_file) {
        if current_version.trim() != TEMPLATE_VERSION {
            notices.push(format!(
                "template version is {}, latest is {}; run `ralph upgrade-prompts`",
                current_version.trim(),
                TEMPLATE_VERSION
            ));
        }
    }

    if changes.is_empty() {
        println!("Doctor found no missing Ralph files.");
    } else {
        println!("Doctor applied {} fix(es):", changes.len());
        for change in &changes {
            println!("- {change}");
        }
    }

    if !notices.is_empty() {
        println!("Doctor notices:");
        for notice in &notices {
            println!("- {notice}");
        }
    }

    Ok(())
}

pub(crate) fn upgrade_prompts(paths: &Paths, templates: &PromptTemplates<'_>) -> Result<()> {
    ensure_beads_initialized(paths)?;
    ensure_layout(paths)?;

    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = paths
        .archive_dir
        .join(format!("prompt-upgrade-{timestamp}-v{TEMPLATE_VERSION}"));
    fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "failed to create prompt backup directory at {}",
            backup_dir.display()
        )
    })?;

    let prompt_files = [
        &paths.meta_prompt_file,
        &paths.issue_prompt_file,
        &paths.cleanup_prompt_file,
        &paths.repair_prompt_file,
        &paths.quality_check_prompt_file,
        &paths.code_review_check_prompt_file,
        &paths.validation_check_prompt_file,
    ];
    for path in prompt_files {
        if path.exists() {
            let backup_target = backup_dir.join(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("template.md"),
            );
            fs::copy(path, &backup_target).with_context(|| {
                format!(
                    "failed to back up {} to {}",
                    path.display(),
                    backup_target.display()
                )
            })?;
        }
    }

    fs::write(&paths.meta_prompt_file, templates.meta).context("failed to upgrade ralph.md")?;
    fs::write(&paths.issue_prompt_file, templates.issue).context("failed to upgrade issue.md")?;
    fs::write(&paths.cleanup_prompt_file, templates.cleanup)
        .context("failed to upgrade cleanup.md")?;
    fs::write(&paths.repair_prompt_file, templates.repair)
        .context("failed to upgrade repair.md")?;
    fs::write(&paths.quality_check_prompt_file, templates.quality_check)
        .context("failed to upgrade quality-check.md")?;
    fs::write(
        &paths.code_review_check_prompt_file,
        templates.code_review_check,
    )
    .context("failed to upgrade code-review-check.md")?;
    fs::write(
        &paths.validation_check_prompt_file,
        templates.validation_check,
    )
    .context("failed to upgrade validation-check.md")?;
    fs::write(&paths.template_version_file, TEMPLATE_VERSION)
        .context("failed to write prompt template version file")?;

    println!(
        "Upgraded prompts to version {} (backup: {})",
        TEMPLATE_VERSION,
        backup_dir.display()
    );
    Ok(())
}

fn ensure_layout(paths: &Paths) -> Result<()> {
    fs::create_dir_all(&paths.archive_dir).context("failed to create .ralph/archive")?;
    fs::create_dir_all(&paths.prompts_dir).context("failed to create .ralph/prompts")?;
    Ok(())
}

fn default_config_contents() -> &'static str {
    "# Ralph configuration (project-local)\n\
# Values here are used when CLI flags are not provided.\n\
\n\
# max_iterations = 10\n\
# reflect_every = 3\n\
# reflect_every_epic = false\n\
# auto_repair_enabled = true\n\
# capture_timeout_seconds = 30\n\
# capture_retries = 1\n\
# claude_timeout_minutes = 30\n\
# claude_retries = 1\n\
# terminal_scrollback_lines = 10000\n\
# close_guardrail_mode = \"warn\" # warn | strict\n\
# snapshot_consistency_enabled = false\n"
}

fn default_agents_shell_contents() -> &'static str {
    "# AGENTS.md\n\
\n\
Repository guidance for coding agents working in this project.\n\
\n\
## Scope\n\
- Applies to this repository unless a deeper AGENTS.md overrides it.\n\
- Use `rules.md` for evolving, run-to-run operating rules; keep this file stable.\n\
\n\
## Build And Test\n\
- Primary build command: `<fill in>`\n\
- Primary test command: `<fill in>`\n\
- Lint/typecheck command(s): `<fill in>`\n\
\n\
## Workflow\n\
- Branch model: `<fill in>`\n\
- Validation required before commit: `<fill in>`\n\
- Required commit/PR conventions: `<fill in>`\n\
\n\
## Repo Invariants\n\
- Architecture constraints and cross-file contracts: `<fill in>`\n\
- Required environment variables/config: `<fill in>`\n\
- Safety/forbidden operations: `<fill in>`\n"
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
