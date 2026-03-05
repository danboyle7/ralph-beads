use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::cli::Paths;

pub(crate) fn print_last_run_summary(paths: &Paths) -> Result<()> {
    for line in last_run_summary_lines(paths)? {
        println!("{line}");
    }
    Ok(())
}

fn last_run_summary_lines(paths: &Paths) -> Result<Vec<String>> {
    if !paths.progress_file.exists() {
        bail!("No progress log found at {}", paths.progress_file.display());
    }

    let content = fs::read_to_string(&paths.progress_file)
        .with_context(|| format!("failed to read {}", paths.progress_file.display()))?;

    let mut run_id = String::from("unknown");
    let mut started = String::from("unknown");
    let mut max_iterations = String::from("unknown");
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    let mut processing = 0_usize;
    let mut final_status = String::from("In progress");
    let mut tail = VecDeque::new();

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("Run ID: ") {
            run_id = value.to_string();
        } else if let Some(value) = line.strip_prefix("Started: ") {
            started = value.to_string();
        } else if let Some(value) = line.strip_prefix("Max Iterations: ") {
            max_iterations = value.to_string();
        }

        if line.contains("Processing issue") {
            processing += 1;
        }
        if line.contains("Completed issue") {
            completed += 1;
        }
        if line.contains("FAILED issue") {
            failed += 1;
        }
        if line.contains("COMPLETE:") || line.contains("STOPPED:") {
            final_status = line.to_string();
        }

        tail.push_back(line.to_string());
        while tail.len() > 8 {
            tail.pop_front();
        }
    }

    let mut lines = vec![
        format!("Run ID: {run_id}"),
        format!("Started: {started}"),
        format!("Max Iterations: {max_iterations}"),
        format!("Issues Started: {processing}"),
        format!("Issues Completed: {completed}"),
        format!("Issues Failed: {failed}"),
        format!("Status: {final_status}"),
        String::new(),
        "Recent Log:".to_string(),
    ];

    for line in tail {
        lines.push(line);
    }

    lines.push(String::new());
    lines.push("Cross-Run Intelligence:".to_string());
    lines.extend(cross_run_lines(paths)?);

    Ok(lines)
}

#[derive(Default)]
struct RunInsights {
    run_id: String,
    total_cost_usd: f64,
    completed_issues: usize,
    validation_total: usize,
    validation_attempt_one_pass: usize,
    retries: usize,
    issue_families: HashMap<String, usize>,
    failure_patterns: HashMap<String, usize>,
}

fn cross_run_lines(paths: &Paths) -> Result<Vec<String>> {
    if !paths.logs_dir.exists() {
        return Ok(vec!["No debug logs found.".to_string()]);
    }

    let mut run_dirs = fs::read_dir(&paths.logs_dir)
        .with_context(|| format!("failed to read {}", paths.logs_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    run_dirs.sort_by_key(|entry| entry.file_name());
    if run_dirs.is_empty() {
        return Ok(vec!["No run directories found.".to_string()]);
    }

    let mut runs = Vec::new();
    for entry in run_dirs {
        if let Ok(run) = parse_run_insights(&entry.path()) {
            runs.push(run);
        }
    }
    if runs.is_empty() {
        return Ok(vec![
            "No semantic/cost data found in run logs yet.".to_string()
        ]);
    }

    let run_count = runs.len() as f64;
    let mut total_retries = 0_usize;
    let mut total_validation = 0_usize;
    let mut total_first_pass = 0_usize;
    let mut total_cost = 0.0_f64;
    let mut total_completed_issues = 0_usize;
    let mut family_totals: HashMap<String, usize> = HashMap::new();
    let mut family_cost_totals: HashMap<String, f64> = HashMap::new();
    let mut family_retry_totals: HashMap<String, usize> = HashMap::new();
    let mut pattern_totals: HashMap<String, usize> = HashMap::new();

    for run in &runs {
        total_retries += run.retries;
        total_validation += run.validation_total;
        total_first_pass += run.validation_attempt_one_pass;
        total_cost += run.total_cost_usd;
        total_completed_issues += run.completed_issues;
        for (family, count) in &run.issue_families {
            *family_totals.entry(family.clone()).or_insert(0) += *count;
            *family_cost_totals.entry(family.clone()).or_insert(0.0) += run.total_cost_usd;
            *family_retry_totals.entry(family.clone()).or_insert(0) += run.retries;
        }
        for (pattern, count) in &run.failure_patterns {
            *pattern_totals.entry(pattern.clone()).or_insert(0) += *count;
        }
    }

    let first_pass_rate = if total_validation == 0 {
        0.0
    } else {
        (total_first_pass as f64 / total_validation as f64) * 100.0
    };
    let mean_retries = total_retries as f64 / run_count;
    let avg_cost_per_issue = if total_completed_issues == 0 {
        0.0
    } else {
        total_cost / total_completed_issues as f64
    };

    let mut lines = vec![
        format!("Runs analyzed: {}", runs.len()),
        format!("Validation first-pass rate: {:.1}%", first_pass_rate),
        format!("Mean retries per run: {:.2}", mean_retries),
        format!(
            "Average cost per completed issue: ${:.4}",
            avg_cost_per_issue
        ),
    ];

    let mut families = family_totals.into_iter().collect::<Vec<_>>();
    families.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    lines.push("Issue family volume:".to_string());
    for (family, count) in families.iter().take(5) {
        let avg_cost = family_cost_totals.get(family).copied().unwrap_or(0.0) / (*count as f64);
        let avg_retries =
            family_retry_totals.get(family).copied().unwrap_or(0) as f64 / (*count as f64);
        lines.push(format!("- {family}: {count} runs"));
        lines.push(format!(
            "  avg_cost=${:.4} avg_retries={:.2}",
            avg_cost, avg_retries
        ));
    }

    let mut patterns = pattern_totals
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .collect::<Vec<_>>();
    patterns.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if patterns.is_empty() {
        lines.push("Repeated failure patterns: none detected.".to_string());
    } else {
        lines.push("Repeated failure patterns:".to_string());
        for (pattern, count) in patterns.iter().take(5) {
            lines.push(format!("- {pattern} ({count}x)"));
        }
        lines.push("Playbook suggestions:".to_string());
        for (pattern, _) in patterns.iter().take(3) {
            lines.push(format!("- {}", playbook_for_pattern(pattern)));
        }
    }

    if let Some(latest) = runs.last() {
        lines.push(String::new());
        lines.push(format!(
            "Latest run ({}) summary: completed_issues={} validations={} retries={} cost=${:.4}",
            latest.run_id,
            latest.completed_issues,
            latest.validation_total,
            latest.retries,
            latest.total_cost_usd
        ));
    }

    Ok(lines)
}

fn parse_run_insights(run_dir: &Path) -> Result<RunInsights> {
    let mut insights = RunInsights {
        run_id: run_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string(),
        ..RunInsights::default()
    };

    parse_semantic_file(run_dir, &mut insights)?;
    parse_cost_file(run_dir, &mut insights)?;

    Ok(insights)
}

fn parse_semantic_file(run_dir: &Path, insights: &mut RunInsights) -> Result<()> {
    let semantic_path = run_dir.join("claude-semantic.ndjson");
    if !semantic_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&semantic_path)
        .with_context(|| format!("failed to read {}", semantic_path.display()))?;

    let mut seen_families = HashSet::new();
    for line in content.lines() {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let event = value.get("event");
        let event_type = event
            .and_then(|event| event.get("type"))
            .and_then(Value::as_str)
            .or_else(|| value.get("event_type").and_then(Value::as_str));

        if let Some(issue_id) = value.get("issue_id").and_then(Value::as_str) {
            let family = issue_family(issue_id);
            if !family.is_empty() && seen_families.insert(family.clone()) {
                *insights.issue_families.entry(family).or_insert(0) += 1;
            }
        }

        match event_type {
            Some("validation_passed") | Some("validation_failed") => {
                insights.validation_total += 1;
                let attempt = event
                    .and_then(|event| event.get("attempt"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
                if event_type == Some("validation_passed") && attempt == 1 {
                    insights.validation_attempt_one_pass += 1;
                }
                if event_type == Some("validation_failed") {
                    let reason = event
                        .and_then(|event| {
                            event
                                .get("reason_full")
                                .and_then(Value::as_str)
                                .or_else(|| event.get("reason").and_then(Value::as_str))
                        })
                        .unwrap_or("validation_failed");
                    let signature = failure_signature(reason);
                    *insights.failure_patterns.entry(signature).or_insert(0) += 1;
                }
            }
            Some("retry_started") => {
                insights.retries += 1;
            }
            Some("tool_finished") => {
                let name = event
                    .and_then(|event| event.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name == "Bash" {
                    let input = event
                        .and_then(|event| event.get("input"))
                        .and_then(Value::as_object)
                        .and_then(|object| object.get("command"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if input.contains("bd close ") {
                        insights.completed_issues += 1;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_cost_file(run_dir: &Path, insights: &mut RunInsights) -> Result<()> {
    let events_path = run_dir.join("claude-events.log");
    if !events_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&events_path)
        .with_context(|| format!("failed to read {}", events_path.display()))?;
    for line in content.lines() {
        let Some(json_start) = line.find('{') else {
            continue;
        };
        let json = &line[json_start..];
        let value: Value = match serde_json::from_str(json) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(Value::as_str) == Some("result") {
            insights.total_cost_usd += value
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
        }
    }
    Ok(())
}

fn issue_family(issue_id: &str) -> String {
    if let Some((prefix, _)) = issue_id.rsplit_once('-') {
        prefix.to_string()
    } else {
        issue_id.to_string()
    }
}

fn failure_signature(reason: &str) -> String {
    let lowered = reason.to_ascii_lowercase();
    if lowered.contains("clippy") {
        return "clippy-lint".to_string();
    }
    if lowered.contains("cannot find type") || lowered.contains("e0433") {
        return "missing-import-or-type".to_string();
    }
    if lowered.contains("exit code 1") && lowered.contains("diff in") {
        return "formatting-drift".to_string();
    }
    if lowered.contains("tool_use_error") || lowered.contains("sibling tool call errored") {
        return "parallel-validation-cascade".to_string();
    }
    reason
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}

fn playbook_for_pattern(pattern: &str) -> String {
    match pattern {
        "clippy-lint" => "Run `cargo clippy` before the full validation chain and apply suggested rewrites immediately.",
        "missing-import-or-type" => "When tests fail with missing symbols, apply compiler-suggested imports before rerunning clippy/tests.",
        "formatting-drift" => "After bulk edits, run `cargo fmt` before check/lint/test to avoid avoidable cascade failures.",
        "parallel-validation-cascade" => "Avoid parallel validation commands when one command failing invalidates sibling calls; run serially for clear causality.",
        _ => "Capture the failure signature in semantic logs and add a targeted pre-check before full validation.",
    }
    .to_string()
}
