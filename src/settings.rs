use std::fs;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::build_info;
use crate::cli::{Cli, CliCommand, Paths};
use crate::init;
use crate::issues;
use crate::run_capture;

const DEFAULT_MAX_ITERATIONS: usize = 10;
const DEFAULT_CAPTURE_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_CAPTURE_RETRIES: usize = 1;
const DEFAULT_CLAUDE_TIMEOUT_MINUTES: u64 = 30;
const DEFAULT_CLAUDE_RETRIES: usize = 1;

#[derive(Debug, Clone, Default)]
pub(crate) struct RalphConfig {
    pub(crate) max_iterations: Option<usize>,
    pub(crate) reflect_every: Option<usize>,
    pub(crate) capture_timeout_seconds: Option<u64>,
    pub(crate) capture_retries: Option<usize>,
    pub(crate) claude_timeout_minutes: Option<u64>,
    pub(crate) claude_retries: Option<usize>,
    pub(crate) close_guardrail_mode: Option<CloseGuardrailMode>,
    pub(crate) snapshot_consistency_enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeSettings {
    pub(crate) max_iterations: usize,
    pub(crate) reflect_every: Option<usize>,
    pub(crate) capture_timeout: Duration,
    pub(crate) capture_retries: usize,
    pub(crate) claude_timeout: Duration,
    pub(crate) claude_retries: usize,
    pub(crate) close_guardrail_mode: CloseGuardrailMode,
    pub(crate) snapshot_consistency_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseGuardrailMode {
    Warn,
    Strict,
}

pub(crate) fn default_runtime_settings() -> RuntimeSettings {
    RuntimeSettings {
        max_iterations: DEFAULT_MAX_ITERATIONS,
        reflect_every: None,
        capture_timeout: Duration::from_secs(DEFAULT_CAPTURE_TIMEOUT_SECONDS),
        capture_retries: DEFAULT_CAPTURE_RETRIES,
        claude_timeout: Duration::from_secs(DEFAULT_CLAUDE_TIMEOUT_MINUTES * 60),
        claude_retries: DEFAULT_CLAUDE_RETRIES,
        close_guardrail_mode: CloseGuardrailMode::Warn,
        snapshot_consistency_enabled: false,
    }
}

pub(crate) fn apply_command_mode(cli: &mut Cli) {
    match cli.command {
        Some(CliCommand::Init) => cli.init = true,
        Some(CliCommand::Doctor) => cli.doctor = true,
        Some(CliCommand::Preflight) => cli.preflight = true,
        Some(CliCommand::UpgradePrompts) => cli.upgrade_prompts = true,
        Some(CliCommand::Summary) => cli.summary = true,
        Some(CliCommand::Cleanup) => cli.cleanup = true,
        Some(CliCommand::Reflect) => cli.reflect = true,
        None => {}
    }
}

pub(crate) fn load_config(paths: &Paths) -> Result<RalphConfig> {
    if !paths.config_file.exists() {
        return Ok(RalphConfig::default());
    }

    let content = fs::read_to_string(&paths.config_file)
        .with_context(|| format!("failed to read {}", paths.config_file.display()))?;
    let mut config = RalphConfig::default();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key {
            "max_iterations" => {
                if let Ok(parsed) = value.parse::<usize>() {
                    config.max_iterations = Some(parsed);
                }
            }
            "reflect_every" => {
                if let Ok(parsed) = value.parse::<usize>() {
                    config.reflect_every = Some(parsed);
                }
            }
            "capture_timeout_seconds" => {
                if let Ok(parsed) = value.parse::<u64>() {
                    config.capture_timeout_seconds = Some(parsed);
                }
            }
            "capture_retries" => {
                if let Ok(parsed) = value.parse::<usize>() {
                    config.capture_retries = Some(parsed);
                }
            }
            "claude_timeout_minutes" => {
                if let Ok(parsed) = value.parse::<u64>() {
                    config.claude_timeout_minutes = Some(parsed);
                }
            }
            "claude_retries" => {
                if let Ok(parsed) = value.parse::<usize>() {
                    config.claude_retries = Some(parsed);
                }
            }
            "close_guardrail_mode" => {
                config.close_guardrail_mode = parse_close_guardrail_mode(value);
            }
            "snapshot_consistency_enabled" => {
                config.snapshot_consistency_enabled = parse_bool(value);
            }
            _ => {}
        }
    }

    Ok(config)
}

pub(crate) fn resolve_runtime_settings(cli: &mut Cli, config: &RalphConfig) -> RuntimeSettings {
    if cli.reflect_every.is_none() {
        cli.reflect_every = config.reflect_every;
    }
    let max_iterations = std::env::var("RALPH_MAX_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .or(cli.iterations)
        .or(config.max_iterations)
        .unwrap_or(DEFAULT_MAX_ITERATIONS);
    let reflect_every = cli.reflect_every.or(config.reflect_every);

    RuntimeSettings {
        max_iterations,
        reflect_every,
        capture_timeout: Duration::from_secs(
            config
                .capture_timeout_seconds
                .unwrap_or(DEFAULT_CAPTURE_TIMEOUT_SECONDS),
        ),
        capture_retries: config.capture_retries.unwrap_or(DEFAULT_CAPTURE_RETRIES),
        claude_timeout: Duration::from_secs(
            config
                .claude_timeout_minutes
                .unwrap_or(DEFAULT_CLAUDE_TIMEOUT_MINUTES)
                .saturating_mul(60),
        ),
        claude_retries: config.claude_retries.unwrap_or(DEFAULT_CLAUDE_RETRIES),
        close_guardrail_mode: config
            .close_guardrail_mode
            .unwrap_or(CloseGuardrailMode::Warn),
        snapshot_consistency_enabled: if cli.snapshot_consistency {
            true
        } else if cli.skip_snapshot_consistency {
            false
        } else {
            config.snapshot_consistency_enabled.unwrap_or(false)
        },
    }
}

pub(crate) fn validate_cli_arguments(cli: &Cli, settings: &RuntimeSettings) -> Result<()> {
    if let Some(every) = settings.reflect_every {
        if every == 0 {
            bail!("--reflect-every must be greater than 0");
        }
    }

    if cli.cleanup && cli.reflect {
        bail!("--cleanup and --reflect cannot be used together");
    }

    if cli.cleanup && cli.reflect_every.is_some() {
        bail!("--cleanup and --reflect-every cannot be used together");
    }

    if cli.reflect && cli.reflect_every.is_some() {
        bail!("--reflect and --reflect-every cannot be used together");
    }

    if cli.snapshot_consistency && cli.skip_snapshot_consistency {
        bail!("--snapshot-consistency and --skip-snapshot-consistency cannot be used together");
    }

    Ok(())
}

pub(crate) fn run_preflight(
    paths: &Paths,
    settings: &RuntimeSettings,
    as_json: bool,
) -> Result<()> {
    let mut checks = Vec::new();

    checks.push(json!({
        "name": "ralph_version",
        "ok": true,
        "detail": build_info::display(),
    }));

    let command_checks = ["claude", "bd", "git"];
    for command in command_checks {
        let ok = which::which(command).is_ok();
        checks.push(json!({
            "name": format!("command:{command}"),
            "ok": ok,
            "detail": if ok { "found in PATH" } else { "missing in PATH" },
        }));
    }

    let beads_ok = paths.project_dir.join(".beads").exists();
    checks.push(json!({
        "name": "beads_layout",
        "ok": beads_ok,
        "detail": if beads_ok { ".beads exists" } else { ".beads missing" },
    }));

    let ralph_ok = paths.ralph_dir.exists();
    checks.push(json!({
        "name": "ralph_layout",
        "ok": ralph_ok,
        "detail": if ralph_ok { ".ralph exists" } else { ".ralph missing (run `ralph init`)" },
    }));

    let prompt_paths = [
        &paths.meta_prompt_file,
        &paths.issue_prompt_file,
        &paths.cleanup_prompt_file,
        &paths.quality_check_prompt_file,
        &paths.code_review_check_prompt_file,
        &paths.validation_check_prompt_file,
    ];
    let prompts_ok = prompt_paths.iter().all(|path| path.exists());
    checks.push(json!({
        "name": "prompt_templates",
        "ok": prompts_ok,
        "detail": if prompts_ok { "all prompt templates present" } else { "one or more prompts missing (run `ralph doctor`)" },
    }));

    let branch = run_capture(["git", "rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let branch_name = branch.trim();
    let branch_ok = !branch_name.is_empty();
    let branch_detail = if !branch_ok {
        "unable to determine current git branch".to_string()
    } else if matches!(branch_name, "main" | "master") {
        "currently on default branch; Ralph should create an issue branch before implementation"
            .to_string()
    } else {
        format!("current branch: {branch_name}")
    };
    checks.push(json!({
        "name": "branch_safety",
        "ok": branch_ok,
        "detail": branch_detail,
    }));

    let bd_ready_ok = run_capture(["bd", "ready", "--json"]).is_ok();
    checks.push(json!({
        "name": "bd_ready",
        "ok": bd_ready_ok,
        "detail": if bd_ready_ok { "`bd ready --json` succeeded" } else { "`bd ready --json` failed" },
    }));

    if settings.snapshot_consistency_enabled {
        match issues::issue_snapshot_consistency_report(paths) {
            Ok(report) => {
                checks.push(json!({
                    "name": "issue_snapshot_consistency",
                    "ok": report.ok,
                    "detail": report.detail,
                    "baseline_count": report.baseline_count,
                    "current_count": report.current_count,
                    "missing_ids": report.missing_ids,
                }));
            }
            Err(error) => {
                checks.push(json!({
                    "name": "issue_snapshot_consistency",
                    "ok": false,
                    "detail": format!("unable to verify issue snapshot consistency: {error}"),
                }));
            }
        }
    } else {
        checks.push(json!({
            "name": "issue_snapshot_consistency",
            "ok": true,
            "detail": "skipped (snapshot_consistency_enabled=false or --skip-snapshot-consistency)",
        }));
    }

    let claude_version = run_capture(["claude", "--version"]).unwrap_or_default();
    let claude_ok = !claude_version.trim().is_empty();
    checks.push(json!({
        "name": "claude_health",
        "ok": claude_ok,
        "detail": if claude_ok {
            format!("claude available: {}", claude_version.trim())
        } else {
            "unable to read `claude --version`".to_string()
        },
    }));

    let template_version = fs::read_to_string(&paths.template_version_file)
        .unwrap_or_else(|_| "missing".to_string())
        .trim()
        .to_string();
    let template_ok = template_version == init::TEMPLATE_VERSION;
    checks.push(json!({
        "name": "template_version",
        "ok": template_ok,
        "detail": if template_ok {
            format!("template version {}", init::TEMPLATE_VERSION)
        } else {
            format!("template version {}; latest is {} (run `ralph upgrade-prompts`)", template_version, init::TEMPLATE_VERSION)
        },
    }));

    let lock_clear = !paths.lock_file.exists();
    checks.push(json!({
        "name": "run_lock",
        "ok": lock_clear,
        "detail": if lock_clear { "no active run lock" } else { "run lock file exists; verify no other Ralph process is active" },
    }));

    let all_ok = checks
        .iter()
        .all(|item| item.get("ok").and_then(Value::as_bool).unwrap_or(false));

    if as_json {
        let report = json!({
            "ok": all_ok,
            "settings": {
                "max_iterations": settings.max_iterations,
                "reflect_every": settings.reflect_every,
                "capture_timeout_seconds": settings.capture_timeout.as_secs(),
                "capture_retries": settings.capture_retries,
                "claude_timeout_seconds": settings.claude_timeout.as_secs(),
                "claude_retries": settings.claude_retries,
                "close_guardrail_mode": close_guardrail_mode_label(settings.close_guardrail_mode),
                "snapshot_consistency_enabled": settings.snapshot_consistency_enabled,
            },
            "checks": checks,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Ralph preflight:");
        for check in &checks {
            let ok = check.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let marker = if ok { "ok" } else { "fail" };
            let name = check.get("name").and_then(Value::as_str).unwrap_or("check");
            let detail = check.get("detail").and_then(Value::as_str).unwrap_or("");
            println!("- [{marker}] {name}: {detail}");
        }
    }

    if all_ok {
        Ok(())
    } else {
        bail!("preflight failed")
    }
}

fn parse_close_guardrail_mode(value: &str) -> Option<CloseGuardrailMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "warn" => Some(CloseGuardrailMode::Warn),
        "strict" => Some(CloseGuardrailMode::Strict),
        _ => None,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn close_guardrail_mode_label(mode: CloseGuardrailMode) -> &'static str {
    match mode {
        CloseGuardrailMode::Warn => "warn",
        CloseGuardrailMode::Strict => "strict",
    }
}
