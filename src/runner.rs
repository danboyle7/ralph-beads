use super::*;

pub(super) fn run_main_loop(cli: Cli, paths: Paths, settings: RuntimeSettings) -> Result<()> {
    let use_tui = !cli.plain && io::stdout().is_terminal();
    let (ui_tx, ui_rx) = mpsc::channel();
    let graceful_quit = Arc::new(AtomicBool::new(false));

    let worker_cli = cli.clone();
    let worker_paths = paths.clone();
    let worker_settings = settings.clone();
    let worker_graceful_quit = Arc::clone(&graceful_quit);
    let worker = thread::spawn(move || {
        worker_main(
            worker_cli,
            worker_paths,
            worker_settings,
            ui_tx,
            worker_graceful_quit,
        )
    });

    let ui_result = if use_tui {
        run_live_tui(ui_rx, graceful_quit)
    } else {
        run_plain_ui(ui_rx)
    };

    let worker_result = match worker.join() {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("worker thread panicked")),
    };

    ui_result?;
    worker_result
}

pub(super) fn worker_main(
    cli: Cli,
    paths: Paths,
    settings: RuntimeSettings,
    ui_tx: Sender<UiEvent>,
    graceful_quit: Arc<AtomicBool>,
) -> Result<()> {
    let _cleanup = CleanupGuard::new(true);

    let announce_startup_step = |message: &str| {
        send(&ui_tx, UiEvent::Status(message.to_string()));
        send(&ui_tx, UiEvent::Activity(message.to_string()));
    };

    announce_startup_step(&format!("Startup: {}", build_info::display()));

    announce_startup_step(
        "Startup: checking prerequisites (commands, .beads/.ralph, prompt files)",
    );
    check_prerequisites(&paths).context("startup failed while checking prerequisites")?;

    if settings.snapshot_consistency_enabled {
        announce_startup_step("Startup: verifying issue snapshot consistency");
        ensure_issue_snapshot_consistency(&paths)
            .context("startup failed while verifying issue snapshot consistency")?;
    } else {
        announce_startup_step("Startup: skipping issue snapshot consistency (disabled)");
    }

    announce_startup_step("Startup: acquiring run lock");
    let _run_lock = acquire_run_lock(&paths).context("startup failed while acquiring run lock")?;

    announce_startup_step("Startup: checking for interrupted issue");
    let interrupted_issue = detect_interrupted_issue(&paths)
        .context("startup failed while checking interrupted work")?;

    announce_startup_step("Startup: archiving previous run (if present)");
    archive_previous_run(&paths, &ui_tx)?;

    announce_startup_step("Startup: initializing progress log");
    let run_id = init_progress_file(&paths, settings.max_iterations)?;
    announce_startup_step("Startup: loading open issue count");
    let mut open_count = get_open_issue_count()?;
    announce_startup_step("Startup: writing issue snapshot baseline");
    write_issue_snapshot(&paths, Some(&run_id))?;
    let total_iterations = if cli.once { 1 } else { settings.max_iterations };
    write_run_state(
        &paths,
        &RunState {
            run_id: run_id.clone(),
            status: "running".to_string(),
            started_at: Local::now().to_rfc3339(),
            updated_at: Local::now().to_rfc3339(),
            current_issue: None,
            iteration: 0,
            total_iterations,
            mode: if cli.cleanup {
                "cleanup".to_string()
            } else if cli.reflect {
                "reflect".to_string()
            } else {
                "loop".to_string()
            },
            pid: std::process::id(),
        },
    )?;
    let _state_guard = RunStateGuard {
        paths: paths.clone(),
        run_id: run_id.clone(),
    };
    let mut debug_logs = if cli.debug {
        Some(DebugLogs::new(&paths, &run_id)?)
    } else {
        None
    };
    let mut completed_issues = 0_usize;
    let failed_issues = 0_usize;
    send(
        &ui_tx,
        UiEvent::Summary(format_run_stats(
            &run_id,
            open_count,
            completed_issues,
            failed_issues,
            0,
            total_iterations,
        )),
    );

    if let Some(logs) = debug_logs.as_mut() {
        let notice = format!(
            "Debug logging enabled: dir={} raw={} activity={} output={} semantic={} report={}",
            logs.run_dir_path.display(),
            logs.raw_events_path.display(),
            logs.activity_path.display(),
            logs.output_path.display(),
            logs.semantic_path.display(),
            logs.report_path.display(),
        );
        logs.log_activity(&notice);
        send(&ui_tx, UiEvent::Activity(notice));
    }

    log_progress(
        &paths,
        &ui_tx,
        format!("Starting Ralph loop with {open_count} open issues"),
    )?;

    if cli.cleanup {
        if interrupted_issue.is_none() {
            log_progress(
                &paths,
                &ui_tx,
                "Cleanup pass skipped: no interrupted issue detected".to_string(),
            )?;
            send(
                &ui_tx,
                UiEvent::Status("Cleanup skipped: no interrupted issue detected".to_string()),
            );
            send(
                &ui_tx,
                UiEvent::Stop("Cleanup pass finished (no interrupted issue found)".to_string()),
            );
            mark_run_state_finished(&paths, &run_id, "stopped")?;
            return Ok(());
        }

        log_progress(&paths, &ui_tx, "Running manual cleanup pass".to_string())?;
        let outcome = run_cleanup_pass(
            &cli,
            &paths,
            &ui_tx,
            &mut debug_logs,
            interrupted_issue.as_deref(),
            "manual flag",
        )?;
        open_count = get_open_issue_count().unwrap_or(0);
        let remaining = get_remaining_issue_count().unwrap_or(open_count);
        send(
            &ui_tx,
            UiEvent::Summary(format_run_stats(
                &run_id,
                open_count,
                completed_issues,
                failed_issues,
                0,
                total_iterations,
            )),
        );
        if matches!(outcome, ClaudeOutcome::CompleteSignal) || remaining == 0 {
            log_progress(
                &paths,
                &ui_tx,
                "COMPLETE: Cleanup pass resolved all open work".to_string(),
            )?;
            mark_run_state_finished(&paths, &run_id, "completed")?;
        } else {
            log_progress(&paths, &ui_tx, "Cleanup pass completed".to_string())?;
            mark_run_state_finished(&paths, &run_id, "stopped")?;
        }
        send(&ui_tx, UiEvent::Stop("Cleanup pass finished".to_string()));
        return Ok(());
    }

    if cli.reflect {
        log_progress(
            &paths,
            &ui_tx,
            "Running manual reflection suite".to_string(),
        )?;
        run_reflection_suite(
            &cli,
            &paths,
            &ui_tx,
            &mut debug_logs,
            "manual --reflect run",
        )?;
        open_count = get_open_issue_count().unwrap_or(0);
        send(
            &ui_tx,
            UiEvent::Summary(format_run_stats(
                &run_id,
                open_count,
                completed_issues,
                failed_issues,
                0,
                total_iterations,
            )),
        );
        log_progress(&paths, &ui_tx, "Reflection suite completed".to_string())?;
        send(
            &ui_tx,
            UiEvent::Stop("Reflection suite finished".to_string()),
        );
        mark_run_state_finished(&paths, &run_id, "completed")?;
        return Ok(());
    }

    if let Some(issue_id) = interrupted_issue {
        send(
            &ui_tx,
            UiEvent::Status(format!("Recovery: interrupted issue {issue_id}")),
        );
        send_activity(
            &ui_tx,
            &mut debug_logs,
            format!("Recovery mode: detected interrupted issue {issue_id}; running cleanup pass"),
        );
        log_progress(
            &paths,
            &ui_tx,
            format!("Detected interrupted issue {issue_id}; running cleanup pass"),
        )?;
        let outcome = run_cleanup_pass(
            &cli,
            &paths,
            &ui_tx,
            &mut debug_logs,
            Some(issue_id.as_str()),
            "auto-detected interrupted issue",
        )?;
        let remaining = get_remaining_issue_count().unwrap_or(1);
        if matches!(outcome, ClaudeOutcome::CompleteSignal) && remaining == 0 {
            log_progress(
                &paths,
                &ui_tx,
                "COMPLETE: Cleanup pass signaled all issues complete".to_string(),
            )?;
            mark_run_state_finished(&paths, &run_id, "completed")?;
            send(
                &ui_tx,
                UiEvent::Stop("Cleanup pass signaled completion".to_string()),
            );
            return Ok(());
        }
        open_count = get_open_issue_count().unwrap_or(open_count);
        send(
            &ui_tx,
            UiEvent::Summary(format_run_stats(
                &run_id,
                open_count,
                completed_issues,
                failed_issues,
                0,
                total_iterations,
            )),
        );
        send(
            &ui_tx,
            UiEvent::Status("Recovery complete; resuming iterations".to_string()),
        );
        log_progress(&paths, &ui_tx, "Auto-cleanup pass completed".to_string())?;
    }

    for iteration in 1..=total_iterations {
        send(
            &ui_tx,
            UiEvent::Status(format!("Iteration {iteration}/{total_iterations}")),
        );
        send(
            &ui_tx,
            UiEvent::Summary(format_run_stats(
                &run_id,
                open_count,
                completed_issues,
                failed_issues,
                iteration,
                total_iterations,
            )),
        );

        if graceful_quit.load(Ordering::Relaxed) {
            log_progress(
                &paths,
                &ui_tx,
                format!("STOPPED: Graceful quit requested before iteration {iteration}"),
            )?;
            send(
                &ui_tx,
                UiEvent::Stop("Graceful stop complete. Exiting before next iteration.".to_string()),
            );
            mark_run_state_finished(&paths, &run_id, "stopped")?;
            return Ok(());
        }

        let issue_id = match get_next_issue()? {
            Some(issue_id) => issue_id,
            None => {
                let remaining = get_remaining_issue_count().unwrap_or(0);
                if remaining > 0 {
                    log_progress(
                        &paths,
                        &ui_tx,
                        format!(
                            "STOPPED: No ready/open issues, but {remaining} non-closed issues remain"
                        ),
                    )?;
                    send(
                        &ui_tx,
                        UiEvent::Stop(format!(
                            "No ready/open issues. {remaining} non-closed issues remain (likely blocked/in_progress)."
                        )),
                    );
                    mark_run_state_finished(&paths, &run_id, "stopped")?;
                    return Ok(());
                }
                log_progress(
                    &paths,
                    &ui_tx,
                    "COMPLETE: No more issues to process".to_string(),
                )?;
                send(
                    &ui_tx,
                    UiEvent::Summary(format_run_stats(
                        &run_id,
                        0,
                        completed_issues,
                        failed_issues,
                        iteration,
                        total_iterations,
                    )),
                );
                send(
                    &ui_tx,
                    UiEvent::Stop("No more issues to process".to_string()),
                );
                mark_run_state_finished(&paths, &run_id, "completed")?;
                return Ok(());
            }
        };

        send(&ui_tx, UiEvent::Issue(issue_id.clone()));
        update_run_state_progress(
            &paths,
            &run_id,
            Some(issue_id.clone()),
            iteration,
            total_iterations,
            "running",
        )?;
        if let Some(logs) = debug_logs.as_mut() {
            logs.set_iteration_context(iteration, &issue_id);
        }
        log_progress(
            &paths,
            &ui_tx,
            format!("Iteration {iteration}: Processing issue {issue_id}"),
        )?;

        let issue_details = get_issue_details(&issue_id)?;
        send(&ui_tx, UiEvent::IssueDetails(issue_details.clone()));
        if cli.verbose {
            send_activity(&ui_tx, &mut debug_logs, format!("Loaded issue {issue_id}"));
        }

        let prompt = build_prompt(&paths, &issue_id, &issue_details);
        let issue_status_before = match get_issue_status_map() {
            Ok(map) => Some(map),
            Err(error) => {
                send_activity(
                    &ui_tx,
                    &mut debug_logs,
                    format!(
                        "WARN: Unable to capture issue status baseline for close guardrail: {error}"
                    ),
                );
                None
            }
        };
        emit_iteration_output_boundary(
            &ui_tx,
            &mut debug_logs,
            iteration,
            total_iterations,
            &issue_id,
        );
        let result = run_claude(&cli, &ui_tx, &issue_id, &prompt, &mut debug_logs)?;

        match result {
            ClaudeOutcome::RateLimited(rate_limit) => {
                let reason = rate_limit.reason();
                let reset_at = rate_limit
                    .reset_at_local()
                    .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let progress_message =
                    format!("STOPPED: Claude rate-limited ({reason}); reset at {reset_at}");
                log_progress(&paths, &ui_tx, progress_message)?;
                send(
                    &ui_tx,
                    UiEvent::Status(format!(
                        "Stopped: Claude rate-limited ({reason}); reset at {reset_at}"
                    )),
                );
                send(
                    &ui_tx,
                    UiEvent::Stop(format!(
                        "Claude rate-limited ({reason}); reset at {reset_at}. Re-run after reset (startup will auto-clean interrupted work if an unfinished issue is detected)."
                    )),
                );
                mark_run_state_finished(&paths, &run_id, "rate_limited")?;
                return Ok(());
            }
            ClaudeOutcome::ErrorResult(error_result) => {
                let progress_message =
                    format!("STOPPED: Claude returned an error result: {error_result}");
                log_progress(&paths, &ui_tx, progress_message)?;
                send(
                    &ui_tx,
                    UiEvent::Stop(format!("Claude returned an error result: {error_result}")),
                );
                mark_run_state_finished(&paths, &run_id, "error")?;
                return Ok(());
            }
            ClaudeOutcome::CompleteSignal => {
                if let Some(before_statuses) = issue_status_before.as_ref() {
                    let continue_run = enforce_single_issue_close_guardrail(
                        &paths,
                        &issue_id,
                        before_statuses,
                        settings.close_guardrail_mode,
                        &ui_tx,
                        &mut debug_logs,
                    )?;
                    if !continue_run {
                        mark_run_state_finished(&paths, &run_id, "stopped")?;
                        return Ok(());
                    }
                }

                let remaining = get_remaining_issue_count().unwrap_or(0);
                if remaining == 0 {
                    completed_issues += 1;
                    open_count = 0;
                    send(
                        &ui_tx,
                        UiEvent::Summary(format_run_stats(
                            &run_id,
                            open_count,
                            completed_issues,
                            failed_issues,
                            iteration,
                            total_iterations,
                        )),
                    );
                    log_progress(
                        &paths,
                        &ui_tx,
                        "COMPLETE: All issues done (signaled by Claude)".to_string(),
                    )?;
                    send(
                        &ui_tx,
                        UiEvent::Stop("Claude signaled completion".to_string()),
                    );
                    mark_run_state_finished(&paths, &run_id, "completed")?;
                    return Ok(());
                }

                completed_issues += 1;
                match get_open_issue_count() {
                    Ok(count) => open_count = count,
                    Err(error) => send_activity(
                        &ui_tx,
                        &mut debug_logs,
                        format!("Unable to refresh open issue count: {error}"),
                    ),
                }
                send(
                    &ui_tx,
                    UiEvent::Summary(format_run_stats(
                        &run_id,
                        open_count,
                        completed_issues,
                        failed_issues,
                        iteration,
                        total_iterations,
                    )),
                );
                send_activity(
                    &ui_tx,
                    &mut debug_logs,
                    format!(
                        "Ignoring premature COMPLETE signal: {remaining} non-closed issues remain"
                    ),
                );
                log_progress(
                    &paths,
                    &ui_tx,
                    format!(
                        "Iteration {iteration}: Completed issue {issue_id} (ignored premature COMPLETE signal; {remaining} non-closed issues remain)"
                    ),
                )?;
            }
            ClaudeOutcome::Success => {
                if let Some(before_statuses) = issue_status_before.as_ref() {
                    let continue_run = enforce_single_issue_close_guardrail(
                        &paths,
                        &issue_id,
                        before_statuses,
                        settings.close_guardrail_mode,
                        &ui_tx,
                        &mut debug_logs,
                    )?;
                    if !continue_run {
                        mark_run_state_finished(&paths, &run_id, "stopped")?;
                        return Ok(());
                    }
                }

                completed_issues += 1;
                match get_open_issue_count() {
                    Ok(count) => open_count = count,
                    Err(error) => send_activity(
                        &ui_tx,
                        &mut debug_logs,
                        format!("Unable to refresh open issue count: {error}"),
                    ),
                }
                send(
                    &ui_tx,
                    UiEvent::Summary(format_run_stats(
                        &run_id,
                        open_count,
                        completed_issues,
                        failed_issues,
                        iteration,
                        total_iterations,
                    )),
                );
                log_progress(
                    &paths,
                    &ui_tx,
                    format!("Iteration {iteration}: Completed issue {issue_id}"),
                )?;
            }
        }

        if let Some(every) = settings.reflect_every {
            if iteration % every == 0 && !graceful_quit.load(Ordering::Relaxed) {
                log_progress(
                    &paths,
                    &ui_tx,
                    format!("Iteration {iteration}: Running scheduled reflection suite"),
                )?;
                run_reflection_suite(
                    &cli,
                    &paths,
                    &ui_tx,
                    &mut debug_logs,
                    &format!("iteration {iteration}/{total_iterations}"),
                )?;
                open_count = get_open_issue_count().unwrap_or(open_count);
                send(
                    &ui_tx,
                    UiEvent::Summary(format_run_stats(
                        &run_id,
                        open_count,
                        completed_issues,
                        failed_issues,
                        iteration,
                        total_iterations,
                    )),
                );
                log_progress(
                    &paths,
                    &ui_tx,
                    format!("Iteration {iteration}: Reflection suite completed"),
                )?;
            }
        }

        if graceful_quit.load(Ordering::Relaxed) {
            log_progress(
                &paths,
                &ui_tx,
                format!("STOPPED: Graceful quit requested after iteration {iteration}"),
            )?;
            send(
                &ui_tx,
                UiEvent::Stop("Graceful stop complete after current iteration.".to_string()),
            );
            mark_run_state_finished(&paths, &run_id, "stopped")?;
            return Ok(());
        }
    }

    log_progress(
        &paths,
        &ui_tx,
        format!("STOPPED: Reached max iterations ({total_iterations})"),
    )?;
    send(
        &ui_tx,
        UiEvent::Stop(format!(
            "Reached max iterations ({total_iterations}) without completion"
        )),
    );
    mark_run_state_finished(&paths, &run_id, "stopped")?;
    Ok(())
}

pub(super) fn format_run_stats(
    run_id: &str,
    open_issues: usize,
    completed_issues: usize,
    failed_issues: usize,
    iteration: usize,
    total_iterations: usize,
) -> String {
    format!(
        "Run {run_id} | Open: {open_issues} | Completed: {completed_issues} | Failed: {failed_issues} | Iteration: {iteration}/{total_iterations}"
    )
}

pub(super) fn check_prerequisites(paths: &Paths) -> Result<()> {
    for command in ["claude", "bd"] {
        which::which(command).with_context(|| format!("{command} not found in PATH"))?;
    }

    if !paths.project_dir.join(".beads").exists() {
        bail!(
            "No .beads directory found in {}",
            paths.project_dir.display()
        );
    }

    if !paths.ralph_dir.exists() {
        bail!(
            "No .ralph directory found in {}",
            paths.project_dir.display()
        );
    }

    if !paths.issue_prompt_file.exists() {
        bail!(
            "Issue prompt not found: expected {}",
            paths.issue_prompt_file.display()
        );
    }

    Ok(())
}

pub(super) fn archive_previous_run(paths: &Paths, ui_tx: &Sender<UiEvent>) -> Result<()> {
    if !paths.progress_file.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&paths.progress_file).unwrap_or_default();
    let line_count = content.lines().count();
    if line_count <= 3 {
        return Ok(());
    }

    let last_run_id = fs::read_to_string(&paths.last_run_file)
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();
    let date_str = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let archive_folder = paths.archive_dir.join(format!("{date_str}-{last_run_id}"));
    fs::create_dir_all(&archive_folder).context("failed to create archive directory")?;
    fs::copy(&paths.progress_file, archive_folder.join("progress.txt"))
        .context("failed to archive progress log")?;
    if paths.state_file.exists() {
        let _ = fs::copy(&paths.state_file, archive_folder.join("state.json"));
    }
    if paths.issue_snapshot_file.exists() {
        let _ = fs::copy(
            &paths.issue_snapshot_file,
            archive_folder.join("issue-snapshot.json"),
        );
    }

    if let Ok(snapshot) = run_capture(["bd", "list", "--all"]) {
        let _ = fs::write(archive_folder.join("beads-snapshot.txt"), snapshot);
    }

    send(
        ui_tx,
        UiEvent::Activity(format!(
            "Archived previous run to {}",
            archive_folder.display()
        )),
    );
    Ok(())
}

pub(super) fn init_progress_file(paths: &Paths, max_iterations: usize) -> Result<String> {
    let run_id = Local::now().format("%Y%m%d-%H%M%S").to_string();
    fs::write(&paths.last_run_file, &run_id).context("failed to write .last-run")?;

    let started = Local::now().to_rfc2822();
    let content = format!(
        "# Ralph Progress Log\nRun ID: {run_id}\nStarted: {started}\nMax Iterations: {max_iterations}\n---\n\n"
    );
    fs::write(&paths.progress_file, content).context("failed to initialize progress file")?;
    Ok(run_id)
}

pub(super) fn get_open_issue_count() -> Result<usize> {
    issues::get_open_issue_count()
}

pub(super) fn get_remaining_issue_count() -> Result<usize> {
    issues::get_remaining_issue_count()
}

pub(super) fn get_issue_status_map() -> Result<HashMap<String, String>> {
    issues::get_issue_status_map()
}

pub(super) fn newly_closed_issue_ids(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<String> {
    issues::newly_closed_issue_ids(before, after)
}

pub(super) fn enforce_single_issue_close_guardrail(
    paths: &Paths,
    issue_id: &str,
    before_statuses: &HashMap<String, String>,
    mode: CloseGuardrailMode,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
) -> Result<bool> {
    if issue_id.starts_with("REFLECT-") || issue_id.starts_with("CLEANUP") {
        send_activity(
            ui_tx,
            debug_logs,
            format!("Close guardrail skipped for non-issue run id `{issue_id}`"),
        );
        return Ok(true);
    }

    let after_statuses = match get_issue_status_map() {
        Ok(map) => map,
        Err(error) => {
            let message = format!("WARN: Unable to verify close guardrail: {error}");
            send_activity(ui_tx, debug_logs, message.clone());
            log_progress(paths, ui_tx, message)?;
            return Ok(true);
        }
    };
    let newly_closed = newly_closed_issue_ids(before_statuses, &after_statuses);
    let expected_closed = newly_closed.iter().any(|id| id == issue_id);
    let unexpected_closed = newly_closed
        .iter()
        .filter(|id| id.as_str() != issue_id)
        .cloned()
        .collect::<Vec<String>>();

    if expected_closed && unexpected_closed.is_empty() {
        return Ok(true);
    }

    let message = if !expected_closed && unexpected_closed.is_empty() {
        format!(
            "Close guardrail violation: expected `{issue_id}` to close this iteration, but it did not."
        )
    } else if expected_closed {
        format!(
            "Close guardrail violation: expected only `{issue_id}` to close, but additional issues closed: {}",
            unexpected_closed.join(", ")
        )
    } else {
        format!(
            "Close guardrail violation: `{issue_id}` did not close and unexpected issues closed: {}",
            unexpected_closed.join(", ")
        )
    };

    match mode {
        CloseGuardrailMode::Warn => {
            let warn = format!("WARN: {message}");
            send_activity(ui_tx, debug_logs, warn.clone());
            log_progress(paths, ui_tx, warn)?;
            Ok(true)
        }
        CloseGuardrailMode::Strict => {
            let stop = format!("STOPPED: {message}");
            send_activity(ui_tx, debug_logs, stop.clone());
            log_progress(paths, ui_tx, stop.clone())?;
            send(
                ui_tx,
                UiEvent::Stop(format!(
                    "{message} Strict close guardrail is enabled; run stopped after this iteration."
                )),
            );
            Ok(false)
        }
    }
}

pub(super) fn run_cleanup_pass(
    cli: &Cli,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    issue_id: Option<&str>,
    trigger: &str,
) -> Result<ClaudeOutcome> {
    let issue_details = issue_id
        .map(get_issue_details)
        .transpose()?
        .unwrap_or_else(|| {
            "No interrupted issue was detected from prior progress logs.".to_string()
        });
    let prompt = build_cleanup_prompt(paths, issue_id, &issue_details, trigger);
    let run_label = issue_id.unwrap_or("none");
    let run_tag = if issue_id.is_some() {
        "CLEANUP"
    } else {
        "CLEANUP-NO-ISSUE"
    };

    if let Some(logs) = debug_logs.as_mut() {
        logs.set_iteration_context(0, run_label);
    }

    emit_named_output_boundary(
        ui_tx,
        debug_logs,
        format!("Cleanup | trigger={trigger} | issue={run_label}"),
    );
    run_claude(cli, ui_tx, run_tag, &prompt, debug_logs)
}

pub(super) fn run_reflection_suite(
    cli: &Cli,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    trigger: &str,
) -> Result<()> {
    send_activity(
        ui_tx,
        debug_logs,
        format!("Starting reflection suite ({trigger})"),
    );
    run_reflection_pass(
        cli,
        paths,
        ui_tx,
        debug_logs,
        ReflectionPassSpec {
            prompt_path: &paths.quality_check_prompt_file,
            fallback_prompt: DEFAULT_QUALITY_CHECK_PROMPT,
            pass_name: "Quality Check",
            pass_id: "REFLECT-QUALITY",
        },
        trigger,
    )?;
    run_reflection_pass(
        cli,
        paths,
        ui_tx,
        debug_logs,
        ReflectionPassSpec {
            prompt_path: &paths.code_review_check_prompt_file,
            fallback_prompt: DEFAULT_CODE_REVIEW_CHECK_PROMPT,
            pass_name: "Code Review Check",
            pass_id: "REFLECT-CODE-REVIEW",
        },
        trigger,
    )?;
    run_reflection_pass(
        cli,
        paths,
        ui_tx,
        debug_logs,
        ReflectionPassSpec {
            prompt_path: &paths.validation_check_prompt_file,
            fallback_prompt: DEFAULT_VALIDATION_CHECK_PROMPT,
            pass_name: "Validation Check",
            pass_id: "REFLECT-VALIDATION",
        },
        trigger,
    )?;
    send_activity(
        ui_tx,
        debug_logs,
        format!("Reflection suite completed ({trigger})"),
    );
    Ok(())
}

pub(super) struct ReflectionPassSpec<'a> {
    pub(super) prompt_path: &'a Path,
    pub(super) fallback_prompt: &'a str,
    pub(super) pass_name: &'a str,
    pub(super) pass_id: &'a str,
}

pub(super) fn run_reflection_pass(
    cli: &Cli,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    spec: ReflectionPassSpec<'_>,
    trigger: &str,
) -> Result<()> {
    let prompt = build_reflection_prompt(
        paths,
        spec.prompt_path,
        spec.fallback_prompt,
        spec.pass_name,
        trigger,
    );
    if let Some(logs) = debug_logs.as_mut() {
        logs.set_iteration_context(0, spec.pass_id);
    }
    emit_named_output_boundary(
        ui_tx,
        debug_logs,
        format!("Reflect | pass={} | trigger={trigger}", spec.pass_name),
    );
    let _ = run_claude(cli, ui_tx, spec.pass_id, &prompt, debug_logs)?;
    Ok(())
}

pub(super) fn detect_interrupted_issue(paths: &Paths) -> Result<Option<String>> {
    if let Some(state) = read_run_state(paths) {
        if state.status.eq_ignore_ascii_case("running") {
            if let Some(issue_id) = state.current_issue {
                if is_non_closed_issue(&issue_id)? {
                    return Ok(Some(issue_id));
                }
            }
        }
    }

    if !paths.progress_file.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&paths.progress_file).unwrap_or_default();
    let mut pending_issue: Option<String> = None;
    for line in content.lines() {
        if let Some(issue_id) = issue_id_from_progress_line(line, "Processing issue ") {
            pending_issue = Some(issue_id);
            continue;
        }
        if let Some(issue_id) = issue_id_from_progress_line(line, "Completed issue ") {
            if pending_issue.as_deref() == Some(issue_id.as_str()) {
                pending_issue = None;
            }
            continue;
        }
        if line.contains("COMPLETE:") {
            pending_issue = None;
        }
    }

    if let Some(issue_id) = pending_issue {
        if is_non_closed_issue(&issue_id)? {
            return Ok(Some(issue_id));
        }
    }

    Ok(None)
}

pub(super) fn is_non_closed_issue(issue_id: &str) -> Result<bool> {
    issues::is_non_closed_issue(issue_id)
}

pub(super) fn issue_id_from_progress_line(line: &str, marker: &str) -> Option<String> {
    let (_, tail) = line.split_once(marker)?;
    tail.split_whitespace().next().map(|id| id.to_string())
}

pub(super) fn log_progress(paths: &Paths, ui_tx: &Sender<UiEvent>, message: String) -> Result<()> {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{timestamp}] {message}");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.progress_file)
        .context("failed to open progress file")?;
    writeln!(file, "{line}").context("failed to append progress log")?;
    send(ui_tx, UiEvent::Progress(line));
    Ok(())
}
