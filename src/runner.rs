use super::*;

struct IterationPauseSummary {
    open_count: usize,
    completed_issues: usize,
    failed_issues: usize,
}

struct RunStatsSummary<'a> {
    run_id: &'a str,
    open_count: usize,
    completed_issues: usize,
    failed_issues: usize,
    iteration: usize,
    total_iterations: usize,
}

struct PauseContext<'a> {
    cli: &'a Cli,
    paths: &'a Paths,
    run_id: &'a str,
    ui_tx: &'a Sender<UiEvent>,
    control_rx: &'a Receiver<WorkerControl>,
    debug_logs: &'a mut Option<DebugLogs>,
}

struct PostRunActionState<'a> {
    stop_line: &'a str,
    pending_status: &'a str,
    summary: RunStatsSummary<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CloseGuardrailOutcome {
    pub(super) continue_run: bool,
    pub(super) issue_closed: bool,
}

fn tool_log_line(message: impl Into<String>) -> String {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    format!("[{timestamp}] {}", message.into())
}

fn stored_last_run_id(paths: &Paths) -> Option<String> {
    let run_id = fs::read_to_string(&paths.last_run_file).ok()?;
    let run_id = run_id.trim();
    if run_id.is_empty() {
        None
    } else {
        Some(run_id.to_string())
    }
}

fn current_run_progress_path(paths: &Paths) -> Result<PathBuf> {
    let run_id = stored_last_run_id(paths).context("failed to determine current run id")?;
    Ok(paths.run_progress_file(&run_id))
}

fn last_run_progress_path(paths: &Paths) -> Option<PathBuf> {
    let run_progress_path =
        stored_last_run_id(paths).map(|run_id| paths.run_progress_file(&run_id));
    if let Some(path) = run_progress_path {
        if path.exists() {
            return Some(path);
        }
    }

    paths
        .progress_file
        .exists()
        .then(|| paths.progress_file.clone())
}

pub(crate) fn read_last_run_progress_content(paths: &Paths) -> Result<Option<String>> {
    let Some(path) = last_run_progress_path(paths) else {
        return Ok(None);
    };

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(content))
}

fn run_progress_header(run_id: &str, started: &str, max_iterations: usize) -> String {
    format!(
        "# Ralph Progress Log\nRun ID: {run_id}\nStarted: {started}\nMax Iterations: {max_iterations}\n---\n\n"
    )
}

fn master_progress_header(run_id: &str, started: &str, max_iterations: usize) -> String {
    let banner = "=".repeat(72);
    format!(
        "{banner}\nRUN START: {run_id}\nStarted: {started}\nMax Iterations: {max_iterations}\n{banner}\n{}\n",
        run_progress_header(run_id, started, max_iterations)
    )
}

fn append_progress_line(path: &Path, line: &str, error_context: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {error_context}"))?;
    writeln!(file, "{line}").with_context(|| format!("failed to append {error_context}"))?;
    Ok(())
}

fn send_tool_log(ui_tx: &Sender<UiEvent>, message: impl Into<String>) {
    send(ui_tx, UiEvent::Progress(tool_log_line(message)));
}

fn record_tool_note(
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    message: impl Into<String>,
) -> Result<()> {
    let message = message.into();
    if let Some(logs) = debug_logs.as_mut() {
        logs.log_activity(&message);
    }
    if paths.progress_file.exists() {
        log_progress(paths, ui_tx, message)
    } else {
        send_tool_log(ui_tx, message);
        Ok(())
    }
}

fn drain_pending_iteration_extensions(
    control_rx: Option<&Receiver<WorkerControl>>,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    summary: &RunStatsSummary<'_>,
    total_iterations: &mut usize,
) -> Result<()> {
    let Some(control_rx) = control_rx else {
        return Ok(());
    };

    let mut additional_iterations = 0_usize;
    loop {
        match control_rx.try_recv() {
            Ok(WorkerControl::ExtendIterations(additional)) => {
                additional_iterations = additional_iterations
                    .checked_add(additional)
                    .context("iteration extension overflowed usize")?;
            }
            Ok(WorkerControl::Reflect) => {
                record_tool_note(
                    paths,
                    ui_tx,
                    debug_logs,
                    "Ignoring reflection request until the current iteration boundary".to_string(),
                )?;
            }
            Ok(WorkerControl::Exit) => {}
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }

    if additional_iterations == 0 {
        return Ok(());
    }

    *total_iterations = total_iterations
        .checked_add(additional_iterations)
        .context("iteration budget overflowed usize")?;
    update_run_state_progress(
        paths,
        summary.run_id,
        None,
        summary.iteration.saturating_sub(1),
        *total_iterations,
        "running",
    )?;
    log_progress(paths, ui_tx, format!("Max Iterations: {total_iterations}"))?;
    log_progress(
        paths,
        ui_tx,
        format!(
            "Queued {additional_iterations} additional iteration{} from the live TUI (new budget: {total_iterations})",
            if additional_iterations == 1 { "" } else { "s" }
        ),
    )?;
    send(
        ui_tx,
        UiEvent::Summary(format_run_stats(
            summary.run_id,
            summary.open_count,
            summary.completed_issues,
            summary.failed_issues,
            summary.iteration.saturating_sub(1),
            *total_iterations,
        )),
    );
    send(
        ui_tx,
        UiEvent::Status(format!(
            "Added {additional_iterations} more iteration{}",
            if additional_iterations == 1 { "" } else { "s" }
        )),
    );

    Ok(())
}

pub(super) fn run_main_loop(cli: Cli, paths: Paths, settings: RuntimeSettings) -> Result<()> {
    let use_tui = !cli.plain && io::stdout().is_terminal();
    let (ui_tx, ui_rx) = mpsc::channel();
    let (control_tx, control_rx) = if use_tui {
        let (tx, rx) = mpsc::channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
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
            control_rx,
            worker_graceful_quit,
        )
    });

    let ui_result = if use_tui {
        run_live_tui(
            ui_rx,
            control_tx.expect("TUI control channel missing"),
            graceful_quit,
            paths.project_dir.clone(),
        )
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
    control_rx: Option<Receiver<WorkerControl>>,
    graceful_quit: Arc<AtomicBool>,
) -> Result<()> {
    let _cleanup = CleanupGuard::new(true);

    let announce_startup_step = |message: &str| {
        send(&ui_tx, UiEvent::Status(message.to_string()));
        send_tool_log(&ui_tx, message.to_string());
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

    let mut total_iterations = if cli.once { 1 } else { settings.max_iterations };

    announce_startup_step("Startup: archiving previous run (if present)");
    archive_previous_run(&paths, &ui_tx)?;

    announce_startup_step("Startup: initializing progress log");
    let run_id = init_progress_file(&paths, total_iterations)?;
    announce_startup_step("Startup: loading open issue count");
    let mut open_count = get_open_issue_count()?;
    announce_startup_step("Startup: writing issue snapshot baseline");
    write_issue_snapshot(&paths, Some(&run_id))?;
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
        send_tool_log(&ui_tx, notice);
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
        record_tool_note(
            &paths,
            &ui_tx,
            &mut debug_logs,
            format!("Recovery mode: detected interrupted issue {issue_id}; running cleanup pass"),
        )?;
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

    let mut iteration = 1_usize;
    loop {
        let iteration_summary = RunStatsSummary {
            run_id: &run_id,
            open_count,
            completed_issues,
            failed_issues,
            iteration,
            total_iterations,
        };
        drain_pending_iteration_extensions(
            control_rx.as_ref(),
            &paths,
            &ui_tx,
            &mut debug_logs,
            &iteration_summary,
            &mut total_iterations,
        )?;
        if iteration > total_iterations {
            if let Some(control_rx) = control_rx.as_ref() {
                let mut pause_ctx = PauseContext {
                    cli: &cli,
                    paths: &paths,
                    run_id: &run_id,
                    ui_tx: &ui_tx,
                    control_rx,
                    debug_logs: &mut debug_logs,
                };
                match wait_for_iteration_extension(
                    &mut pause_ctx,
                    total_iterations,
                    IterationPauseSummary {
                        open_count,
                        completed_issues,
                        failed_issues,
                    },
                )? {
                    Some(additional_iterations) => {
                        total_iterations += additional_iterations;
                        continue;
                    }
                    None => {
                        mark_run_state_finished(&paths, &run_id, "stopped")?;
                        return Ok(());
                    }
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
            return Ok(());
        }

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
                    if settings.auto_repair_enabled {
                        let trigger =
                            "auto-repair: no ready non-epic issue found while work remains";
                        send(
                            &ui_tx,
                            UiEvent::Status("No ready work found; running repair pass".to_string()),
                        );
                        record_tool_note(
                            &paths,
                            &ui_tx,
                            &mut debug_logs,
                            format!(
                                "No ready issue found with {remaining} non-closed issues remaining; running repair pass"
                            ),
                        )?;
                        log_progress(
                            &paths,
                            &ui_tx,
                            format!("Iteration {iteration}: Running repair pass ({trigger})"),
                        )?;
                        let outcome = run_repair_pass(
                            &cli,
                            &paths,
                            &ui_tx,
                            &mut debug_logs,
                            trigger,
                            remaining,
                        )?;
                        match &outcome {
                            ClaudeOutcome::RateLimited(rate_limit) => {
                                let reason = rate_limit.reason();
                                let reset_at = rate_limit
                                    .reset_at_local()
                                    .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
                                    .unwrap_or_else(|| "unknown".to_string());
                                let progress_message = format!(
                                    "STOPPED: Repair pass rate-limited by Claude ({reason}); reset at {reset_at}"
                                );
                                log_progress(&paths, &ui_tx, progress_message)?;
                                send(
                                    &ui_tx,
                                    UiEvent::Status(format!(
                                        "Stopped: repair pass rate-limited ({reason}); reset at {reset_at}"
                                    )),
                                );
                                send(
                                    &ui_tx,
                                    UiEvent::Stop(format!(
                                        "Repair pass rate-limited ({reason}); reset at {reset_at}"
                                    )),
                                );
                                mark_run_state_finished(&paths, &run_id, "rate_limited")?;
                                return Ok(());
                            }
                            ClaudeOutcome::ErrorResult(error_result) => {
                                log_progress(
                                    &paths,
                                    &ui_tx,
                                    format!(
                                        "STOPPED: Repair pass returned an error result: {error_result}"
                                    ),
                                )?;
                                send(
                                    &ui_tx,
                                    UiEvent::Stop(format!(
                                        "Repair pass returned an error result: {error_result}"
                                    )),
                                );
                                mark_run_state_finished(&paths, &run_id, "error")?;
                                return Ok(());
                            }
                            ClaudeOutcome::Success | ClaudeOutcome::CompleteSignal => {}
                        }
                        open_count = get_open_issue_count().unwrap_or(open_count);
                        let remaining_after = get_remaining_issue_count().unwrap_or(open_count);
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
                        if matches!(outcome, ClaudeOutcome::CompleteSignal) || remaining_after == 0
                        {
                            log_progress(
                                &paths,
                                &ui_tx,
                                "COMPLETE: Repair pass resolved all remaining work".to_string(),
                            )?;
                            if let Some(control_rx) = control_rx.as_ref() {
                                let mut pause_ctx = PauseContext {
                                    cli: &cli,
                                    paths: &paths,
                                    run_id: &run_id,
                                    ui_tx: &ui_tx,
                                    control_rx,
                                    debug_logs: &mut debug_logs,
                                };
                                if let Some(additional_iterations) = wait_for_post_run_action(
                                    &mut pause_ctx,
                                    PostRunActionState {
                                        stop_line: "Repair pass resolved all remaining work",
                                        pending_status: "completed",
                                        summary: RunStatsSummary {
                                            run_id: run_id.as_str(),
                                            open_count: 0,
                                            completed_issues,
                                            failed_issues,
                                            iteration,
                                            total_iterations,
                                        },
                                    },
                                )? {
                                    total_iterations += additional_iterations;
                                    iteration += 1;
                                    open_count = get_open_issue_count().unwrap_or(open_count);
                                    continue;
                                }
                            } else {
                                send(
                                    &ui_tx,
                                    UiEvent::Stop(
                                        "Repair pass resolved all remaining work".to_string(),
                                    ),
                                );
                            }
                            mark_run_state_finished(&paths, &run_id, "completed")?;
                            return Ok(());
                        }

                        if let Some(repaired_issue_id) = get_next_issue()? {
                            send(
                                &ui_tx,
                                UiEvent::Status(
                                    "Repair pass completed; resuming iterations".to_string(),
                                ),
                            );
                            log_progress(
                                &paths,
                                &ui_tx,
                                format!(
                                    "Iteration {iteration}: Repair pass completed; ready issue {repaired_issue_id} is now available"
                                ),
                            )?;
                            repaired_issue_id
                        } else {
                            let stop_line = format!(
                                "Repair pass completed, but no ready/open issues were found. {remaining_after} non-closed issues remain."
                            );
                            log_progress(
                                &paths,
                                &ui_tx,
                                format!(
                                    "STOPPED: Repair pass did not produce a ready issue; {remaining_after} non-closed issues remain"
                                ),
                            )?;
                            if let Some(control_rx) = control_rx.as_ref() {
                                let mut pause_ctx = PauseContext {
                                    cli: &cli,
                                    paths: &paths,
                                    run_id: &run_id,
                                    ui_tx: &ui_tx,
                                    control_rx,
                                    debug_logs: &mut debug_logs,
                                };
                                if let Some(additional_iterations) = wait_for_post_run_action(
                                    &mut pause_ctx,
                                    PostRunActionState {
                                        stop_line: &stop_line,
                                        pending_status: "stopped",
                                        summary: RunStatsSummary {
                                            run_id: run_id.as_str(),
                                            open_count,
                                            completed_issues,
                                            failed_issues,
                                            iteration,
                                            total_iterations,
                                        },
                                    },
                                )? {
                                    total_iterations += additional_iterations;
                                    iteration += 1;
                                    open_count = get_open_issue_count().unwrap_or(open_count);
                                    continue;
                                }
                            } else {
                                send(&ui_tx, UiEvent::Stop(stop_line));
                            }
                            mark_run_state_finished(&paths, &run_id, "stopped")?;
                            return Ok(());
                        }
                    } else {
                        let stop_line = format!(
                        "No ready/open issues. {remaining} non-closed issues remain (likely blocked/in_progress)."
                    );
                        log_progress(
                        &paths,
                        &ui_tx,
                        format!(
                            "STOPPED: No ready/open issues, but {remaining} non-closed issues remain"
                        ),
                    )?;
                        if let Some(control_rx) = control_rx.as_ref() {
                            let mut pause_ctx = PauseContext {
                                cli: &cli,
                                paths: &paths,
                                run_id: &run_id,
                                ui_tx: &ui_tx,
                                control_rx,
                                debug_logs: &mut debug_logs,
                            };
                            if let Some(additional_iterations) = wait_for_post_run_action(
                                &mut pause_ctx,
                                PostRunActionState {
                                    stop_line: &stop_line,
                                    pending_status: "stopped",
                                    summary: RunStatsSummary {
                                        run_id: run_id.as_str(),
                                        open_count,
                                        completed_issues,
                                        failed_issues,
                                        iteration,
                                        total_iterations,
                                    },
                                },
                            )? {
                                total_iterations += additional_iterations;
                                iteration += 1;
                                open_count = get_open_issue_count().unwrap_or(open_count);
                                continue;
                            }
                        } else {
                            send(&ui_tx, UiEvent::Stop(stop_line));
                        }
                        mark_run_state_finished(&paths, &run_id, "stopped")?;
                        return Ok(());
                    }
                } else {
                    let stop_line = "No more issues to process".to_string();
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
                    if let Some(control_rx) = control_rx.as_ref() {
                        let mut pause_ctx = PauseContext {
                            cli: &cli,
                            paths: &paths,
                            run_id: &run_id,
                            ui_tx: &ui_tx,
                            control_rx,
                            debug_logs: &mut debug_logs,
                        };
                        if let Some(additional_iterations) = wait_for_post_run_action(
                            &mut pause_ctx,
                            PostRunActionState {
                                stop_line: &stop_line,
                                pending_status: "completed",
                                summary: RunStatsSummary {
                                    run_id: run_id.as_str(),
                                    open_count: 0,
                                    completed_issues,
                                    failed_issues,
                                    iteration,
                                    total_iterations,
                                },
                            },
                        )? {
                            total_iterations += additional_iterations;
                            iteration += 1;
                            open_count = get_open_issue_count().unwrap_or(open_count);
                            continue;
                        }
                    } else {
                        send(&ui_tx, UiEvent::Stop(stop_line));
                    }
                    mark_run_state_finished(&paths, &run_id, "completed")?;
                    return Ok(());
                }
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
            record_tool_note(
                &paths,
                &ui_tx,
                &mut debug_logs,
                format!("Loaded issue {issue_id}"),
            )?;
        }

        let prompt = build_prompt(&paths, &issue_id, &issue_details);
        let issue_status_before = if cli.dry_run {
            record_tool_note(
                &paths,
                &ui_tx,
                &mut debug_logs,
                "Dry run: skipping close guardrail verification for this iteration".to_string(),
            )?;
            None
        } else {
            match get_issue_status_map() {
                Ok(map) => Some(map),
                Err(error) => {
                    record_tool_note(
                        &paths,
                        &ui_tx,
                        &mut debug_logs,
                        format!(
                            "WARN: Unable to capture issue status baseline for close guardrail: {error}"
                        ),
                    )?;
                    None
                }
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
                let guardrail_outcome = if let Some(before_statuses) = issue_status_before.as_ref()
                {
                    enforce_single_issue_close_guardrail(
                        &paths,
                        &issue_id,
                        before_statuses,
                        settings.close_guardrail_mode,
                        &ui_tx,
                        &mut debug_logs,
                    )?
                } else {
                    CloseGuardrailOutcome {
                        continue_run: true,
                        issue_closed: true,
                    }
                };
                if !guardrail_outcome.continue_run {
                    mark_run_state_finished(&paths, &run_id, "stopped")?;
                    return Ok(());
                }

                let remaining = get_remaining_issue_count().unwrap_or(0);
                if remaining == 0 {
                    completed_issues += 1;
                    let closed_epics = if settings.reflect_every_epic {
                        newly_closed_epic_ids(&paths, &issue_status_before, &ui_tx, &mut debug_logs)
                    } else {
                        Vec::new()
                    };
                    if !closed_epics.is_empty() && !graceful_quit.load(Ordering::Relaxed) {
                        let reason_text = format!("epic completed ({})", closed_epics.join(", "));
                        log_progress(
                            &paths,
                            &ui_tx,
                            format!(
                                "Iteration {iteration}: Running reflection suite ({reason_text})"
                            ),
                        )?;
                        run_reflection_suite(
                            &cli,
                            &paths,
                            &ui_tx,
                            &mut debug_logs,
                            &format!("iteration {iteration}/{total_iterations}; {reason_text}"),
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
                    if let Some(control_rx) = control_rx.as_ref() {
                        let mut pause_ctx = PauseContext {
                            cli: &cli,
                            paths: &paths,
                            run_id: &run_id,
                            ui_tx: &ui_tx,
                            control_rx,
                            debug_logs: &mut debug_logs,
                        };
                        if let Some(additional_iterations) = wait_for_post_run_action(
                            &mut pause_ctx,
                            PostRunActionState {
                                stop_line: "Claude signaled completion",
                                pending_status: "completed",
                                summary: RunStatsSummary {
                                    run_id: run_id.as_str(),
                                    open_count,
                                    completed_issues,
                                    failed_issues,
                                    iteration,
                                    total_iterations,
                                },
                            },
                        )? {
                            total_iterations += additional_iterations;
                            iteration += 1;
                            open_count = get_open_issue_count().unwrap_or(open_count);
                            continue;
                        }
                    } else {
                        send(
                            &ui_tx,
                            UiEvent::Stop("Claude signaled completion".to_string()),
                        );
                    }
                    mark_run_state_finished(&paths, &run_id, "completed")?;
                    return Ok(());
                }

                match get_open_issue_count() {
                    Ok(count) => open_count = count,
                    Err(error) => record_tool_note(
                        &paths,
                        &ui_tx,
                        &mut debug_logs,
                        format!("Unable to refresh open issue count: {error}"),
                    )?,
                }
                if guardrail_outcome.issue_closed {
                    completed_issues += 1;
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
                record_tool_note(
                    &paths,
                    &ui_tx,
                    &mut debug_logs,
                    format!(
                        "Ignoring premature COMPLETE signal: {remaining} non-closed issues remain"
                    ),
                )?;
                if guardrail_outcome.issue_closed {
                    log_progress(
                        &paths,
                        &ui_tx,
                        format!(
                            "Iteration {iteration}: Completed issue {issue_id} (ignored premature COMPLETE signal; {remaining} non-closed issues remain)"
                        ),
                    )?;
                } else {
                    log_progress(
                        &paths,
                        &ui_tx,
                        format!(
                            "Iteration {iteration}: Claude signaled COMPLETE, but {issue_id} remains open after the close guardrail warning; {remaining} non-closed issues remain"
                        ),
                    )?;
                }
            }
            ClaudeOutcome::Success => {
                let guardrail_outcome = if let Some(before_statuses) = issue_status_before.as_ref()
                {
                    enforce_single_issue_close_guardrail(
                        &paths,
                        &issue_id,
                        before_statuses,
                        settings.close_guardrail_mode,
                        &ui_tx,
                        &mut debug_logs,
                    )?
                } else {
                    CloseGuardrailOutcome {
                        continue_run: true,
                        issue_closed: true,
                    }
                };
                if !guardrail_outcome.continue_run {
                    mark_run_state_finished(&paths, &run_id, "stopped")?;
                    return Ok(());
                }

                match get_open_issue_count() {
                    Ok(count) => open_count = count,
                    Err(error) => record_tool_note(
                        &paths,
                        &ui_tx,
                        &mut debug_logs,
                        format!("Unable to refresh open issue count: {error}"),
                    )?,
                }
                if guardrail_outcome.issue_closed {
                    completed_issues += 1;
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
                if guardrail_outcome.issue_closed {
                    log_progress(
                        &paths,
                        &ui_tx,
                        format!("Iteration {iteration}: Completed issue {issue_id}"),
                    )?;
                } else {
                    log_progress(
                        &paths,
                        &ui_tx,
                        format!(
                            "Iteration {iteration}: Claude returned success, but {issue_id} remains open after the close guardrail warning"
                        ),
                    )?;
                }
            }
        }

        let closed_epics = if settings.reflect_every_epic {
            newly_closed_epic_ids(&paths, &issue_status_before, &ui_tx, &mut debug_logs)
        } else {
            Vec::new()
        };
        let iteration_reflection_due = settings
            .reflect_every
            .is_some_and(|every| iteration.is_multiple_of(every));
        let should_reflect = (!closed_epics.is_empty() || iteration_reflection_due)
            && !graceful_quit.load(Ordering::Relaxed);

        if should_reflect {
            let mut reasons = Vec::new();
            if !closed_epics.is_empty() {
                reasons.push(format!("epic completed ({})", closed_epics.join(", ")));
            }
            if iteration_reflection_due {
                reasons.push("scheduled interval".to_string());
            }
            let reason_text = reasons.join("; ");
            log_progress(
                &paths,
                &ui_tx,
                format!("Iteration {iteration}: Running reflection suite ({reason_text})"),
            )?;
            run_reflection_suite(
                &cli,
                &paths,
                &ui_tx,
                &mut debug_logs,
                &format!("iteration {iteration}/{total_iterations}; {reason_text}"),
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

        iteration += 1;
    }
}

fn wait_for_iteration_extension(
    ctx: &mut PauseContext<'_>,
    total_iterations: usize,
    summary: IterationPauseSummary,
) -> Result<Option<usize>> {
    update_run_state_progress(
        ctx.paths,
        ctx.run_id,
        None,
        total_iterations,
        total_iterations,
        "waiting_for_input",
    )?;
    log_progress(
        ctx.paths,
        ctx.ui_tx,
        format!("Paused after reaching max iterations ({total_iterations}); waiting for TUI input"),
    )?;
    send(ctx.ui_tx, UiEvent::IterationBudgetReached(total_iterations));

    loop {
        match ctx.control_rx.recv() {
            Ok(WorkerControl::ExtendIterations(additional_iterations)) => {
                let new_total_iterations = total_iterations + additional_iterations;
                update_run_state_progress(
                    ctx.paths,
                    ctx.run_id,
                    None,
                    total_iterations,
                    new_total_iterations,
                    "running",
                )?;
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!("Max Iterations: {new_total_iterations}"),
                )?;
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!(
                        "Resuming run with {additional_iterations} additional iteration{} (new budget: {new_total_iterations})",
                        if additional_iterations == 1 { "" } else { "s" }
                    ),
                )?;
                record_tool_note(
                    ctx.paths,
                    ctx.ui_tx,
                    ctx.debug_logs,
                    format!(
                        "Iteration budget extended by {additional_iterations}; next iteration will be {}/{}",
                        total_iterations + 1,
                        new_total_iterations
                    ),
                )?;
                send(
                    ctx.ui_tx,
                    UiEvent::Summary(format_run_stats(
                        ctx.run_id,
                        summary.open_count,
                        summary.completed_issues,
                        summary.failed_issues,
                        total_iterations,
                        new_total_iterations,
                    )),
                );
                send(
                    ctx.ui_tx,
                    UiEvent::Status(format!(
                        "Resuming with {additional_iterations} more iteration{}",
                        if additional_iterations == 1 { "" } else { "s" }
                    )),
                );
                return Ok(Some(additional_iterations));
            }
            Ok(WorkerControl::Reflect) => {
                send(
                    ctx.ui_tx,
                    UiEvent::Status("Running reflection suite".to_string()),
                );
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!(
                        "Paused after reaching max iterations ({total_iterations}); running reflection suite from TUI"
                    ),
                )?;
                run_reflection_suite(
                    ctx.cli,
                    ctx.paths,
                    ctx.ui_tx,
                    ctx.debug_logs,
                    &format!(
                        "manual TUI reflection while paused at iteration budget {total_iterations}"
                    ),
                )?;
                send(
                    ctx.ui_tx,
                    UiEvent::Summary(format_run_stats(
                        ctx.run_id,
                        summary.open_count,
                        summary.completed_issues,
                        summary.failed_issues,
                        total_iterations,
                        total_iterations,
                    )),
                );
                update_run_state_progress(
                    ctx.paths,
                    ctx.run_id,
                    None,
                    total_iterations,
                    total_iterations,
                    "waiting_for_input",
                )?;
                send(
                    ctx.ui_tx,
                    UiEvent::Status("Waiting for iteration input".to_string()),
                );
                send(ctx.ui_tx, UiEvent::IterationBudgetReached(total_iterations));
            }
            Ok(WorkerControl::Exit) | Err(_) => {
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!("STOPPED: Reached max iterations ({total_iterations})"),
                )?;
                send(
                    ctx.ui_tx,
                    UiEvent::Stop(format!(
                        "Reached max iterations ({total_iterations}) without completion"
                    )),
                );
                return Ok(None);
            }
        }
    }
}

fn wait_for_post_run_action(
    ctx: &mut PauseContext<'_>,
    state: PostRunActionState<'_>,
) -> Result<Option<usize>> {
    update_run_state_progress(
        ctx.paths,
        ctx.run_id,
        None,
        state.summary.iteration,
        state.summary.total_iterations,
        "waiting_for_input",
    )?;
    send(ctx.ui_tx, UiEvent::Stop(state.stop_line.to_string()));
    send(ctx.ui_tx, UiEvent::PostRunReflectAvailable);

    loop {
        match ctx.control_rx.recv() {
            Ok(WorkerControl::ExtendIterations(additional_iterations)) => {
                let new_total_iterations = state.summary.total_iterations + additional_iterations;
                update_run_state_progress(
                    ctx.paths,
                    ctx.run_id,
                    None,
                    state.summary.iteration,
                    new_total_iterations,
                    "running",
                )?;
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!("Max Iterations: {new_total_iterations}"),
                )?;
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!(
                        "Resuming finished run with {additional_iterations} additional iteration{} (next iteration: {}/{new_total_iterations})",
                        if additional_iterations == 1 { "" } else { "s" },
                        state.summary.iteration + 1,
                    ),
                )?;
                record_tool_note(
                    ctx.paths,
                    ctx.ui_tx,
                    ctx.debug_logs,
                    format!(
                        "Post-run TUI resumed the run with {additional_iterations} additional iteration{}",
                        if additional_iterations == 1 { "" } else { "s" }
                    ),
                )?;
                send(
                    ctx.ui_tx,
                    UiEvent::Summary(format_run_stats(
                        state.summary.run_id,
                        state.summary.open_count,
                        state.summary.completed_issues,
                        state.summary.failed_issues,
                        state.summary.iteration,
                        new_total_iterations,
                    )),
                );
                send(
                    ctx.ui_tx,
                    UiEvent::Status(format!(
                        "Resuming with {additional_iterations} more iteration{}",
                        if additional_iterations == 1 { "" } else { "s" }
                    )),
                );
                return Ok(Some(additional_iterations));
            }
            Ok(WorkerControl::Reflect) => {
                send(
                    ctx.ui_tx,
                    UiEvent::Status("Running reflection suite".to_string()),
                );
                log_progress(
                    ctx.paths,
                    ctx.ui_tx,
                    format!(
                        "Post-run state: running reflection suite from TUI ({})",
                        state.pending_status
                    ),
                )?;
                run_reflection_suite(
                    ctx.cli,
                    ctx.paths,
                    ctx.ui_tx,
                    ctx.debug_logs,
                    &format!(
                        "manual TUI reflection after run ended ({})",
                        state.pending_status
                    ),
                )?;
                let refreshed_open_count =
                    get_open_issue_count().unwrap_or(state.summary.open_count);
                send(
                    ctx.ui_tx,
                    UiEvent::Summary(format_run_stats(
                        state.summary.run_id,
                        refreshed_open_count,
                        state.summary.completed_issues,
                        state.summary.failed_issues,
                        state.summary.iteration,
                        state.summary.total_iterations,
                    )),
                );
                update_run_state_progress(
                    ctx.paths,
                    ctx.run_id,
                    None,
                    state.summary.iteration,
                    state.summary.total_iterations,
                    "waiting_for_input",
                )?;
                send(ctx.ui_tx, UiEvent::Stop(state.stop_line.to_string()));
                send(ctx.ui_tx, UiEvent::PostRunReflectAvailable);
            }
            Ok(WorkerControl::Exit) | Err(_) => return Ok(None),
        }
    }
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
    let Some(last_run_id) = stored_last_run_id(paths) else {
        return Ok(());
    };

    let run_progress_file = paths.run_progress_file(&last_run_id);
    let legacy_progress = if !run_progress_file.exists() && paths.progress_file.exists() {
        let content = fs::read_to_string(&paths.progress_file).unwrap_or_default();
        (content.lines().count() > 3).then_some(content)
    } else {
        None
    };
    let beads_snapshot = run_capture(["bd", "list", "--all", "--limit", "0"]).ok();
    let should_archive = legacy_progress.is_some()
        || paths.state_file.exists()
        || paths.issue_snapshot_file.exists()
        || beads_snapshot.is_some();
    if !should_archive {
        return Ok(());
    }

    let archive_folder = paths.run_archive_dir(&last_run_id);
    fs::create_dir_all(&archive_folder).context("failed to create archive directory")?;
    if legacy_progress.is_some() {
        fs::copy(&paths.progress_file, &run_progress_file)
            .context("failed to archive progress log")?;
    }
    if paths.state_file.exists() {
        let _ = fs::copy(&paths.state_file, archive_folder.join("state.json"));
    }
    if paths.issue_snapshot_file.exists() {
        let _ = fs::copy(
            &paths.issue_snapshot_file,
            archive_folder.join("issue-snapshot.json"),
        );
    }

    if let Some(snapshot) = beads_snapshot {
        let _ = fs::write(archive_folder.join("beads-snapshot.txt"), snapshot);
    }

    send_tool_log(
        ui_tx,
        format!("Archived previous run to {}", archive_folder.display()),
    );
    Ok(())
}

pub(super) fn init_progress_file(paths: &Paths, max_iterations: usize) -> Result<String> {
    let run_id = Local::now().format("%Y%m%d-%H%M%S").to_string();
    fs::write(&paths.last_run_file, &run_id).context("failed to write .last-run")?;

    let started = Local::now().to_rfc2822();
    let run_content = run_progress_header(&run_id, &started, max_iterations);
    let run_progress_file = paths.run_progress_file(&run_id);
    fs::create_dir_all(paths.run_archive_dir(&run_id))
        .context("failed to create per-run archive directory")?;
    fs::write(&run_progress_file, run_content).context("failed to initialize run progress file")?;

    let master_content = master_progress_header(&run_id, &started, max_iterations);
    let has_existing_master = paths.progress_file.exists()
        && fs::metadata(&paths.progress_file)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.progress_file)
        .context("failed to open master progress log")?;
    if has_existing_master {
        writeln!(file).context("failed to separate master progress runs")?;
    }
    write!(file, "{master_content}").context("failed to initialize master progress log")?;
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

pub(super) fn get_issue_type_map() -> Result<HashMap<String, String>> {
    issues::get_issue_type_map()
}

pub(super) fn get_issue_type(issue_id: &str) -> Result<Option<String>> {
    issues::get_issue_type(issue_id)
}

pub(super) fn newly_closed_issue_ids(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<String> {
    issues::newly_closed_issue_ids(before, after)
}

pub(super) fn newly_closed_epic_ids(
    paths: &Paths,
    before_statuses: &Option<HashMap<String, String>>,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
) -> Vec<String> {
    let Some(before) = before_statuses.as_ref() else {
        return Vec::new();
    };

    let after = match get_issue_status_map() {
        Ok(map) => map,
        Err(error) => {
            record_tool_note(
                paths,
                ui_tx,
                debug_logs,
                format!("WARN: Unable to detect newly closed epics: {error}"),
            )
            .ok();
            return Vec::new();
        }
    };
    let newly_closed = newly_closed_issue_ids(before, &after);
    if newly_closed.is_empty() {
        return Vec::new();
    }

    let issue_types = match get_issue_type_map() {
        Ok(map) => map,
        Err(error) => {
            record_tool_note(
                paths,
                ui_tx,
                debug_logs,
                format!("WARN: Unable to load issue types for epic reflection: {error}"),
            )
            .ok();
            return Vec::new();
        }
    };

    let mut epic_ids = newly_closed
        .into_iter()
        .filter(|id| {
            let issue_type = issue_types
                .get(id)
                .cloned()
                .or_else(|| match get_issue_type(id) {
                    Ok(value) => value,
                    Err(error) => {
                        record_tool_note(
                            paths,
                            ui_tx,
                            debug_logs,
                            format!("WARN: Unable to look up type for `{id}`: {error}"),
                        )
                        .ok();
                        None
                    }
                });
            issue_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("epic"))
        })
        .collect::<Vec<String>>();
    epic_ids.sort();
    epic_ids.dedup();
    epic_ids
}

pub(super) fn enforce_single_issue_close_guardrail(
    paths: &Paths,
    issue_id: &str,
    before_statuses: &HashMap<String, String>,
    mode: CloseGuardrailMode,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
) -> Result<CloseGuardrailOutcome> {
    if issue_id.starts_with("REFLECT-") || issue_id.starts_with("CLEANUP") {
        record_tool_note(
            paths,
            ui_tx,
            debug_logs,
            format!("Close guardrail skipped for non-issue run id `{issue_id}`"),
        )?;
        return Ok(CloseGuardrailOutcome {
            continue_run: true,
            issue_closed: true,
        });
    }

    let after_statuses = match get_issue_status_map() {
        Ok(map) => map,
        Err(error) => {
            let message = format!("WARN: Unable to verify close guardrail: {error}");
            record_tool_note(paths, ui_tx, debug_logs, message.clone())?;
            return Ok(CloseGuardrailOutcome {
                continue_run: true,
                issue_closed: true,
            });
        }
    };
    let newly_closed = newly_closed_issue_ids(before_statuses, &after_statuses);
    let issue_closed = newly_closed.iter().any(|id| id == issue_id);
    let unexpected_closed = newly_closed
        .iter()
        .filter(|id| id.as_str() != issue_id)
        .cloned()
        .collect::<Vec<String>>();

    if issue_closed && unexpected_closed.is_empty() {
        return Ok(CloseGuardrailOutcome {
            continue_run: true,
            issue_closed: true,
        });
    }

    let message = close_guardrail_violation_message(issue_id, issue_closed, &unexpected_closed);

    match mode {
        CloseGuardrailMode::Warn => {
            let warn = format!("WARN: {message}");
            record_tool_note(paths, ui_tx, debug_logs, warn.clone())?;
            Ok(CloseGuardrailOutcome {
                continue_run: true,
                issue_closed,
            })
        }
        CloseGuardrailMode::Strict => {
            let stop = format!("STOPPED: {message}");
            record_tool_note(paths, ui_tx, debug_logs, stop.clone())?;
            send(
                ui_tx,
                UiEvent::Stop(format!(
                    "{message} Strict close guardrail is enabled; run stopped after this iteration."
                )),
            );
            Ok(CloseGuardrailOutcome {
                continue_run: false,
                issue_closed,
            })
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

pub(super) fn run_repair_pass(
    cli: &Cli,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    trigger: &str,
    remaining_count: usize,
) -> Result<ClaudeOutcome> {
    let prompt = build_repair_prompt(paths, trigger, remaining_count);

    if let Some(logs) = debug_logs.as_mut() {
        logs.set_iteration_context(0, "repair");
    }

    emit_named_output_boundary(
        ui_tx,
        debug_logs,
        format!("Repair | trigger={trigger} | remaining={remaining_count}"),
    );
    run_claude(cli, ui_tx, "REPAIR", &prompt, debug_logs)
}

pub(super) fn run_reflection_suite(
    cli: &Cli,
    paths: &Paths,
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    trigger: &str,
) -> Result<()> {
    record_tool_note(
        paths,
        ui_tx,
        debug_logs,
        format!("Starting reflection suite ({trigger})"),
    )?;
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
    record_tool_note(
        paths,
        ui_tx,
        debug_logs,
        format!("Reflection suite completed ({trigger})"),
    )?;
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
    let outcome = run_claude(cli, ui_tx, spec.pass_id, &prompt, debug_logs)?;
    if let Some(message) = reflection_failure_message(spec.pass_name, &outcome) {
        record_tool_note(paths, ui_tx, debug_logs, format!("STOPPED: {message}"))?;
        send(ui_tx, UiEvent::Stop(message.clone()));
        return Err(anyhow::anyhow!(message));
    }
    Ok(())
}

fn close_guardrail_violation_message(
    issue_id: &str,
    issue_closed: bool,
    unexpected_closed: &[String],
) -> String {
    if !issue_closed && unexpected_closed.is_empty() {
        format!(
            "Close guardrail violation: expected `{issue_id}` to close this iteration, but it did not."
        )
    } else if issue_closed {
        format!(
            "Close guardrail violation: expected only `{issue_id}` to close, but additional issues closed: {}",
            unexpected_closed.join(", ")
        )
    } else {
        format!(
            "Close guardrail violation: `{issue_id}` did not close and unexpected issues closed: {}",
            unexpected_closed.join(", ")
        )
    }
}

fn reflection_failure_message(pass_name: &str, outcome: &ClaudeOutcome) -> Option<String> {
    match outcome {
        ClaudeOutcome::Success | ClaudeOutcome::CompleteSignal => None,
        ClaudeOutcome::RateLimited(rate_limit) => {
            let reason = rate_limit.reason();
            let reset_at = rate_limit
                .reset_at_local()
                .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            Some(format!(
                "Reflection pass `{pass_name}` rate-limited by Claude ({reason}); reset at {reset_at}"
            ))
        }
        ClaudeOutcome::ErrorResult(error_result) => Some(format!(
            "Reflection pass `{pass_name}` returned an error result: {error_result}"
        )),
    }
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

    let Some(content) = read_last_run_progress_content(paths)? else {
        return Ok(None);
    };
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
    let run_progress_file = current_run_progress_path(paths)?;
    append_progress_line(&run_progress_file, &line, "run progress log")?;
    append_progress_line(&paths.progress_file, &line, "master progress log")?;
    send(ui_tx, UiEvent::Progress(line));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_guardrail_message_reports_issue_still_open() {
        let message = close_guardrail_violation_message("BD-123", false, &[]);

        assert!(message.contains("expected `BD-123` to close this iteration, but it did not"));
    }

    #[test]
    fn issue_id_from_progress_line_extracts_first_token() {
        let line = "[2026-03-10 12:00:00] Iteration 3: Processing issue BD-123 extra words";

        assert_eq!(
            issue_id_from_progress_line(line, "Processing issue "),
            Some("BD-123".to_string())
        );
    }

    #[test]
    fn reflection_failure_message_reports_error_results() {
        let outcome = ClaudeOutcome::ErrorResult("validation failed".to_string());

        let message =
            reflection_failure_message("Validation Check", &outcome).expect("message expected");

        assert!(message.contains("Validation Check"));
        assert!(message.contains("validation failed"));
    }

    #[test]
    fn reflection_failure_message_allows_successful_passes() {
        assert!(reflection_failure_message("Quality Check", &ClaudeOutcome::Success).is_none());
        assert!(
            reflection_failure_message("Quality Check", &ClaudeOutcome::CompleteSignal).is_none()
        );
    }
}
