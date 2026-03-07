use super::*;
pub(super) enum ClaudeOutcome {
    Success,
    CompleteSignal,
    RateLimited(ClaudeRateLimitEvent),
    ErrorResult(String),
}

#[derive(Clone, Debug)]
pub(super) struct ClaudeRateLimitEvent {
    status: Option<String>,
    limit_type: Option<String>,
    overage_status: Option<String>,
    overage_reason: Option<String>,
    reset_at_epoch: Option<i64>,
}

impl ClaudeRateLimitEvent {
    pub(super) fn reset_at_local(&self) -> Option<DateTime<Local>> {
        let timestamp = self.reset_at_epoch?;
        let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)?;
        Some(utc.with_timezone(&Local))
    }

    pub(super) fn reason(&self) -> String {
        self.overage_reason
            .as_deref()
            .or(self.overage_status.as_deref())
            .or(self.status.as_deref())
            .or(self.limit_type.as_deref())
            .unwrap_or("unknown")
            .to_string()
    }

    pub(super) fn is_blocking(&self) -> bool {
        !matches!(self.status.as_deref(), Some("allowed"))
    }
}

pub(super) fn run_claude(
    cli: &Cli,
    ui_tx: &Sender<UiEvent>,
    issue_id: &str,
    prompt: &str,
    debug_logs: &mut Option<DebugLogs>,
) -> Result<ClaudeOutcome> {
    FULL_ACTIVITY_TEXT.store(cli.verbose, Ordering::Relaxed);

    if cli.dry_run {
        send_tool_log(ui_tx, debug_logs, format!("Dry run for issue {issue_id}"));
        for line in prompt.lines() {
            send(ui_tx, UiEvent::Output(line.to_string()));
        }
        return Ok(ClaudeOutcome::Success);
    }

    let max_retries = runtime_settings().claude_retries;
    let mut attempt = 0_usize;
    loop {
        match run_claude_once(cli, ui_tx, issue_id, prompt, debug_logs) {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                let retryable =
                    attempt < max_retries && is_transient_error_text(&error.to_string());
                if !retryable {
                    return Err(error);
                }
                attempt += 1;
                send_tool_log(
                    ui_tx,
                    debug_logs,
                    format!(
                        "Claude transient failure on {issue_id}; retrying attempt {}/{}",
                        attempt, max_retries
                    ),
                );
                thread::sleep(Duration::from_millis(500 * attempt as u64));
            }
        }
    }
}

fn run_claude_once(
    cli: &Cli,
    ui_tx: &Sender<UiEvent>,
    issue_id: &str,
    prompt: &str,
    debug_logs: &mut Option<DebugLogs>,
) -> Result<ClaudeOutcome> {
    FULL_ACTIVITY_TEXT.store(cli.verbose, Ordering::Relaxed);
    send_tool_log(ui_tx, debug_logs, format!("Running Claude on {issue_id}"));
    send_tool_log(ui_tx, debug_logs, "Using structured Claude stream");

    let current_dir = std::env::current_dir().context("failed to determine cwd")?;
    let mut child = Command::new("claude")
        .args([
            "--dangerously-skip-permissions",
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "-",
        ])
        .current_dir(&current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start claude")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .context("failed to write prompt to claude stdin")?;
    }

    let (stream_tx, stream_rx) = mpsc::channel();

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, false, stream_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, true, stream_tx.clone());
    }
    drop(stream_tx);

    let mut collected = String::new();
    let mut visible_output = String::new();
    let mut render_state = ClaudeRenderState::default();
    let mut stdout_decoder = ClaudeStreamDecoder::default();
    let mut stderr_decoder = ClaudeStreamDecoder::default();
    let timeout = runtime_settings().claude_timeout;
    let started = Instant::now();
    loop {
        match stream_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(message) => {
                collected.push_str(&message.raw);
                process_claude_chunk(
                    &message.raw,
                    message.is_stderr,
                    if message.is_stderr {
                        &mut stderr_decoder
                    } else {
                        &mut stdout_decoder
                    },
                    &mut render_state,
                    ui_tx,
                    &mut visible_output,
                    debug_logs,
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("Claude timed out after {}s", timeout.as_secs());
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    for line in stdout_decoder.finish() {
        process_claude_line(
            &line,
            false,
            &mut render_state,
            ui_tx,
            &mut visible_output,
            debug_logs,
        );
    }
    for line in stderr_decoder.finish() {
        process_claude_line(
            &line,
            true,
            &mut render_state,
            ui_tx,
            &mut visible_output,
            debug_logs,
        );
    }

    send(ui_tx, UiEvent::Spinner(None));

    let status = child.wait().context("failed to wait on claude")?;
    if !status.success() {
        bail!("Claude exited with status {}", status);
    }

    if let Some(error_result) = render_state.error_result.take() {
        return Ok(ClaudeOutcome::ErrorResult(error_result));
    }

    if let Some(rate_limit) = render_state.rate_limit_event.take() {
        if !render_state.saw_success_result && rate_limit.is_blocking() {
            return Ok(ClaudeOutcome::RateLimited(rate_limit));
        }
    }

    if visible_output.contains("<promise>COMPLETE</promise>")
        || collected.contains("<promise>COMPLETE</promise>")
    {
        Ok(ClaudeOutcome::CompleteSignal)
    } else {
        Ok(ClaudeOutcome::Success)
    }
}

struct StreamMessage {
    raw: String,
    is_stderr: bool,
}

fn spawn_reader<R: io::Read + Send + 'static>(
    reader: R,
    is_stderr: bool,
    tx: Sender<StreamMessage>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = [0_u8; 4096];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let chunk = String::from_utf8_lossy(&buffer[..count]).to_string();
                    let _ = tx.send(StreamMessage {
                        raw: chunk,
                        is_stderr,
                    });
                }
                Err(_) => break,
            }
        }
    });
}

#[derive(Default)]
struct ClaudeStreamDecoder {
    buffer: String,
}

#[derive(Default)]
struct ClaudeRenderState {
    saw_partial_text: bool,
    saw_any_text: bool,
    ends_with_newline: bool,
    streamed_text_block_indexes: HashSet<u64>,
    usage_tracker: UsageTracker,
    tool_lifecycle: ToolLifecycleTracker,
    phase_tracker: RunPhaseTracker,
    rate_limit_event: Option<ClaudeRateLimitEvent>,
    error_result: Option<String>,
    saw_success_result: bool,
}

#[derive(Default)]
struct UsageTracker {
    by_message_id: HashMap<String, UsageTally>,
    by_actor: HashMap<String, UsageTally>,
}

#[derive(Default)]
struct ToolLifecycleTracker {
    by_tool_id: HashMap<String, ToolCallState>,
    by_block_index: HashMap<u64, String>,
}

struct ToolCallState {
    actor: String,
    name: String,
    started_at: Instant,
    input_buffer: String,
    input_value: Option<Value>,
}

struct CompletedToolCall {
    actor: String,
    name: String,
    duration_ms: u128,
    input: Option<Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunPhase {
    Discover,
    Implement,
    Validate,
    Finalize,
}

impl RunPhase {
    fn as_str(self) -> &'static str {
        match self {
            RunPhase::Discover => "discover",
            RunPhase::Implement => "implement",
            RunPhase::Validate => "validate",
            RunPhase::Finalize => "finalize",
        }
    }
}

#[derive(Default)]
struct RunPhaseTracker {
    current: Option<RunPhase>,
    validation_attempts: HashMap<String, usize>,
    pending_validation_error: Option<String>,
}

#[derive(Default)]
struct SemanticEventBundle {
    activities: Vec<String>,
    output_lines: Vec<String>,
    diff_lines: Vec<String>,
    timeline_lines: Vec<String>,
    subagent_lines: Vec<String>,
    tool_updates: Vec<ToolCallUiUpdate>,
    subagent_updates: Vec<SubagentUiUpdate>,
    machine_records: Vec<Value>,
}

struct RenderedDiff {
    file_path: Option<String>,
    hunk_count: usize,
    lines: Vec<String>,
    truncated: bool,
}

impl UsageTracker {
    fn apply_sample(
        &mut self,
        message_id: Option<String>,
        actor_key: String,
        sample: UsageTally,
    ) -> UsageTally {
        if let Some(message_id) = message_id {
            return usage_delta_for_key(&mut self.by_message_id, message_id, sample);
        }
        usage_delta_for_key(&mut self.by_actor, actor_key, sample)
    }
}

impl ToolLifecycleTracker {
    fn observe_stream_tool_start(&mut self, root: &Value, event: &Value, started_at: Instant) {
        let block = match event.get("content_block") {
            Some(block) => block,
            None => return,
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return;
        }

        let index = match event.get("index").and_then(Value::as_u64) {
            Some(index) => index,
            None => return,
        };
        let tool_id = match block.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return,
        };
        let actor = actor_label(root);
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let input = block.get("input");

        self.by_block_index.insert(index, tool_id.clone());
        self.upsert_tool_call(&tool_id, actor, name, input, started_at);
    }

    fn observe_stream_tool_input_delta(&mut self, event: &Value) {
        let index = match event.get("index").and_then(Value::as_u64) {
            Some(index) => index,
            None => return,
        };
        let partial = event
            .get("delta")
            .and_then(|delta| delta.get("partial_json"))
            .and_then(Value::as_str);
        let Some(partial) = partial else {
            return;
        };
        let Some(tool_id) = self.by_block_index.get(&index) else {
            return;
        };
        let Some(call) = self.by_tool_id.get_mut(tool_id) else {
            return;
        };
        call.input_buffer.push_str(partial);
    }

    fn observe_stream_tool_block_stop(
        &mut self,
        event: &Value,
    ) -> Option<(String, String, Option<Value>)> {
        let index = event.get("index").and_then(Value::as_u64)?;
        let tool_id = self.by_block_index.remove(&index)?;
        let call = self.by_tool_id.get_mut(&tool_id)?;
        if call.input_value.is_none() && !call.input_buffer.trim().is_empty() {
            if let Ok(input) = serde_json::from_str::<Value>(&call.input_buffer) {
                call.input_value = Some(input);
            }
        }
        Some((tool_id, call.name.clone(), call.input_value.clone()))
    }

    fn observe_assistant_tool_uses(&mut self, root: &Value, started_at: Instant) {
        let content = root
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array);
        let Some(content) = content else {
            return;
        };

        let actor = actor_label(root);
        for item in content {
            if item.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(tool_id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let input = item.get("input");
            self.upsert_tool_call(tool_id, actor.clone(), name, input, started_at);
        }
    }

    fn complete_tool_call(
        &mut self,
        tool_use_id: &str,
        completed_at: Instant,
    ) -> Option<CompletedToolCall> {
        let call = self.by_tool_id.remove(tool_use_id)?;
        let duration_ms = completed_at.duration_since(call.started_at).as_millis();
        Some(CompletedToolCall {
            actor: call.actor,
            name: call.name,
            duration_ms,
            input: call.input_value,
        })
    }

    fn upsert_tool_call(
        &mut self,
        tool_id: &str,
        actor: String,
        name: String,
        input: Option<&Value>,
        started_at: Instant,
    ) {
        let entry = self
            .by_tool_id
            .entry(tool_id.to_string())
            .or_insert_with(|| ToolCallState {
                actor: actor.clone(),
                name: name.clone(),
                started_at,
                input_buffer: String::new(),
                input_value: None,
            });

        if entry.name == "unknown" {
            entry.name = name;
        }
        if entry.actor == "claude" && actor != "claude" {
            entry.actor = actor;
        }
        if let Some(input) = input {
            if should_store_tool_input(input) {
                entry.input_value = Some(input.clone());
            }
        }
    }
}

fn should_store_tool_input(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

impl RunPhaseTracker {
    fn transition_to(&mut self, next: RunPhase) -> Option<RunPhase> {
        if self.current == Some(next) {
            None
        } else {
            self.current = Some(next);
            Some(next)
        }
    }

    fn next_validation_attempt(&mut self, key: &str) -> usize {
        let attempt = self.validation_attempts.entry(key.to_string()).or_insert(0);
        *attempt += 1;
        *attempt
    }

    fn remember_validation_error(&mut self, excerpt: Option<&str>) {
        self.pending_validation_error = excerpt.map(ToOwned::to_owned);
    }

    fn clear_validation_error(&mut self) {
        self.pending_validation_error = None;
    }

    fn take_validation_error(&mut self) -> Option<String> {
        self.pending_validation_error.take()
    }
}

fn usage_delta_for_key(
    map: &mut HashMap<String, UsageTally>,
    key: String,
    sample: UsageTally,
) -> UsageTally {
    let previous = map.get(&key).copied().unwrap_or_default();
    map.insert(key, sample);
    UsageTally::delta_from_previous(previous, sample)
}

impl ClaudeStreamDecoder {
    fn push_chunk(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut lines = Vec::new();

        while let Some(index) = self.buffer.find('\n') {
            let mut line: String = self.buffer.drain(..=index).collect();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
        }

        lines
    }

    fn finish(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            Vec::new()
        } else {
            let line = self.buffer.trim_end_matches('\r').to_string();
            self.buffer.clear();
            vec![line]
        }
    }
}

fn process_claude_chunk(
    chunk: &str,
    is_stderr: bool,
    decoder: &mut ClaudeStreamDecoder,
    render_state: &mut ClaudeRenderState,
    ui_tx: &Sender<UiEvent>,
    visible_output: &mut String,
    debug_logs: &mut Option<DebugLogs>,
) {
    for line in decoder.push_chunk(chunk) {
        process_claude_line(
            &line,
            is_stderr,
            render_state,
            ui_tx,
            visible_output,
            debug_logs,
        );
    }
}

fn process_claude_line(
    line: &str,
    is_stderr: bool,
    render_state: &mut ClaudeRenderState,
    ui_tx: &Sender<UiEvent>,
    visible_output: &mut String,
    debug_logs: &mut Option<DebugLogs>,
) {
    if line.trim().is_empty() {
        return;
    }

    if let Some(logs) = debug_logs.as_mut() {
        logs.log_raw_event(is_stderr, line);
    }

    if let Ok(value) = serde_json::from_str::<Value>(line) {
        let event = stream_event_value(&value);
        observe_terminal_conditions(&value, render_state);

        if let Some(delta) = extract_usage_delta(&value, event, render_state) {
            if !delta.is_zero() {
                send(ui_tx, UiEvent::UsageDelta(delta));
            }
        }
        if let Some(cost) = extract_cost_delta(&value) {
            if cost > 0.0 {
                send(ui_tx, UiEvent::CostDelta(cost));
            }
        }

        if should_stop_spinner(&value, event) {
            send(ui_tx, UiEvent::Spinner(None));
        }

        if let Some(label) = spinner_label_for_event(&value, event) {
            send(ui_tx, UiEvent::Spinner(Some(label)));
        }

        let semantic_events = semantic_activity_events(&value, event, render_state);

        if let Some(logs) = debug_logs.as_mut() {
            for record in &semantic_events.machine_records {
                logs.log_semantic_value(record);
            }
        }

        for activity in semantic_events.activities {
            if let Some(logs) = debug_logs.as_mut() {
                logs.log_semantic_line("activity", &activity);
            }
            send_activity(ui_tx, debug_logs, activity);
        }

        for line in semantic_events.output_lines {
            if let Some(logs) = debug_logs.as_mut() {
                logs.log_semantic_line("output", &line);
                logs.log_report_line(&line);
            }
            send_output_line(ui_tx, debug_logs, line.clone());
            visible_output.push_str(&line);
            visible_output.push('\n');
            render_state.saw_any_text = true;
            render_state.ends_with_newline = true;
        }

        for line in semantic_events.diff_lines {
            if let Some(logs) = debug_logs.as_mut() {
                logs.log_semantic_line("diff", &line);
            }
            send(ui_tx, UiEvent::Diff(line.clone()));
            send(ui_tx, UiEvent::Output(format!("Δ {line}")));
        }

        for line in semantic_events.timeline_lines {
            if let Some(logs) = debug_logs.as_mut() {
                logs.log_semantic_line("timeline", &line);
            }
            send(ui_tx, UiEvent::Timeline(line));
        }

        for line in semantic_events.subagent_lines {
            if let Some(logs) = debug_logs.as_mut() {
                logs.log_semantic_line("subagent", &line);
            }
            send(ui_tx, UiEvent::Subagent(line));
        }

        for update in semantic_events.tool_updates {
            send(ui_tx, UiEvent::ToolCall(update));
        }

        for update in semantic_events.subagent_updates {
            send(ui_tx, UiEvent::SubagentCall(update));
        }

        if let Some(activity) = activity_for_event(&value, event) {
            send_activity(ui_tx, debug_logs, activity);
        }

        if let Some(text) = extract_stream_text(&value, event, render_state) {
            visible_output.push_str(&text);
            render_state.saw_any_text = true;
            render_state.ends_with_newline = text.ends_with('\n');
            send(ui_tx, UiEvent::Spinner(None));
            send_output_chunk(ui_tx, debug_logs, text);
        }
        return;
    }

    let mut text = String::new();
    if is_stderr {
        text.push_str("[stderr] ");
    }
    text.push_str(line);
    text.push('\n');
    visible_output.push_str(&text);
    render_state.saw_any_text = true;
    render_state.ends_with_newline = true;
    send_output_chunk(ui_tx, debug_logs, text);
}

fn stream_event_value(value: &Value) -> Option<&Value> {
    if value.get("type").and_then(Value::as_str) == Some("stream_event") {
        value.get("event")
    } else {
        Some(value)
    }
}

fn observe_terminal_conditions(root: &Value, render_state: &mut ClaudeRenderState) {
    let root_type = root.get("type").and_then(Value::as_str);

    if root_type == Some("rate_limit_event") {
        render_state.rate_limit_event = Some(parse_rate_limit_event(root));
        return;
    }

    if root_type == Some("result") && root.get("is_error").and_then(Value::as_bool) == Some(true) {
        let error_text = root
            .get("result")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Claude returned an error result")
            .to_string();
        render_state.error_result = Some(error_text);
        return;
    }

    if root_type == Some("result") && root.get("is_error").and_then(Value::as_bool) == Some(false) {
        render_state.saw_success_result = true;
    }
}

fn parse_rate_limit_event(root: &Value) -> ClaudeRateLimitEvent {
    let info = root.get("rate_limit_info");

    ClaudeRateLimitEvent {
        status: info
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        limit_type: info
            .and_then(|value| value.get("rateLimitType"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        overage_status: info
            .and_then(|value| value.get("overageStatus"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        overage_reason: info
            .and_then(|value| value.get("overageDisabledReason"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        reset_at_epoch: info
            .and_then(|value| value.get("resetsAt"))
            .and_then(Value::as_i64),
    }
}

fn extract_stream_text(
    root: &Value,
    event: Option<&Value>,
    render_state: &mut ClaudeRenderState,
) -> Option<String> {
    let root_type = root.get("type").and_then(Value::as_str);

    if let Some(event) = event {
        let event_type = event.get("type").and_then(Value::as_str);

        if event_type == Some("content_block_delta") {
            if let Some(delta) = event.get("delta") {
                let delta_type = delta.get("type").and_then(Value::as_str);
                if matches!(delta_type, Some("text_delta" | "text")) {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            render_state.saw_partial_text = true;
                            if let Some(index) = event.get("index").and_then(Value::as_u64) {
                                render_state.streamed_text_block_indexes.insert(index);
                            }
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        if event_type == Some("content_block_start") {
            if let Some(content_block) = event.get("content_block") {
                if content_block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = content_block.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            render_state.saw_partial_text = true;
                            if let Some(index) = event.get("index").and_then(Value::as_u64) {
                                render_state.streamed_text_block_indexes.insert(index);
                            }
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        if event_type == Some("content_block_stop") {
            if let Some(index) = event.get("index").and_then(Value::as_u64) {
                if render_state.streamed_text_block_indexes.remove(&index)
                    && !render_state.ends_with_newline
                {
                    return Some("\n".to_string());
                }
            }
        }

        if event_type == Some("message_stop")
            && render_state.saw_partial_text
            && !render_state.ends_with_newline
        {
            return Some("\n".to_string());
        }
    }

    if root_type == Some("assistant") {
        let should_emit_full_message = !render_state.saw_partial_text;
        render_state.saw_partial_text = false;
        if should_emit_full_message {
            if let Some(text) = extract_text_blocks(
                root.get("message")
                    .and_then(|message| message.get("content"))
                    .or_else(|| root.get("content")),
            ) {
                return Some(ensure_trailing_newline(text));
            }
        }
    }

    if root_type == Some("result") && !render_state.saw_any_text {
        if let Some(text) = root.get("result").and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(ensure_trailing_newline(text.to_string()));
            }
        }
    }

    None
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn extract_text_blocks(value: Option<&Value>) -> Option<String> {
    let content = value?.as_array()?;
    let mut text = String::new();

    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(chunk) = item.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn usage_from_usage_value(usage: Option<&Value>) -> Option<UsageTally> {
    let usage = usage?.as_object()?;
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let tally = UsageTally {
        input_tokens,
        output_tokens,
        cache_write_tokens,
        cache_read_tokens,
    };

    if tally.is_zero() {
        None
    } else {
        Some(tally)
    }
}

fn usage_actor_key(root: &Value) -> String {
    root.get("parent_tool_use_id")
        .and_then(Value::as_str)
        .map(|id| format!("subagent:{id}"))
        .unwrap_or_else(|| "claude".to_string())
}

fn extract_usage_delta(
    root: &Value,
    event: Option<&Value>,
    render_state: &mut ClaudeRenderState,
) -> Option<UsageTally> {
    let actor_key = usage_actor_key(root);

    if let Some(event) = event {
        if let Some(message) = event.get("message") {
            if let Some(sample) = usage_from_usage_value(message.get("usage")) {
                let message_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                return Some(
                    render_state
                        .usage_tracker
                        .apply_sample(message_id, actor_key, sample),
                );
            }
        }

        if let Some(sample) = usage_from_usage_value(event.get("usage")) {
            let message_id = event
                .get("message_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    event
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .or_else(|| {
                    root.get("message")
                        .and_then(|message| message.get("id"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                });
            return Some(
                render_state
                    .usage_tracker
                    .apply_sample(message_id, actor_key, sample),
            );
        }
    }

    if let Some(message) = root.get("message") {
        if let Some(sample) = usage_from_usage_value(message.get("usage")) {
            let message_id = message
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            return Some(
                render_state
                    .usage_tracker
                    .apply_sample(message_id, actor_key, sample),
            );
        }
    }

    if let Some(sample) = usage_from_usage_value(root.get("usage")) {
        let message_id = root
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        return Some(
            render_state
                .usage_tracker
                .apply_sample(message_id, actor_key, sample),
        );
    }

    None
}

fn extract_cost_delta(root: &Value) -> Option<f64> {
    if root.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    root.get("total_cost_usd").and_then(Value::as_f64)
}

fn semantic_activity_events(
    root: &Value,
    event: Option<&Value>,
    render_state: &mut ClaudeRenderState,
) -> SemanticEventBundle {
    let mut bundle = SemanticEventBundle::default();
    let now = Instant::now();
    let root_type = root.get("type").and_then(Value::as_str);

    if let Some(event) = event {
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                if let Some(block) = event.get("content_block") {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let tool_use_id =
                            block.get("id").and_then(Value::as_str).unwrap_or("unknown");
                        let tool_name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let actor = actor_label(root);
                        let mut timeline_line = format!(
                            "{} | tool_started {} ({})",
                            actor,
                            compact_text(tool_use_id, 16),
                            tool_name
                        );
                        let input_focus = summarize_tool_call_input(tool_name, block.get("input"));
                        if let Some(focus) = input_focus.as_deref() {
                            timeline_line.push_str(" | ");
                            timeline_line.push_str(focus);
                        }
                        bundle.timeline_lines.push(timeline_line);
                        bundle.tool_updates.push(ToolCallUiUpdate {
                            tool_use_id: tool_use_id.to_string(),
                            actor: actor.clone(),
                            name: tool_name.to_string(),
                            status: LiveCallStatus::Running,
                            duration_ms: None,
                            detail: input_focus,
                        });
                        if tool_name == "Agent" {
                            bundle.subagent_updates.push(SubagentUiUpdate {
                                tool_use_id: tool_use_id.to_string(),
                                status: LiveCallStatus::Running,
                                model: None,
                                preview: Some("starting...".to_string()),
                                summary: None,
                                duration_ms: None,
                            });
                        }
                        bundle.machine_records.push(json!({
                            "type": "tool_started",
                            "actor": actor,
                            "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                            "tool_use_id": tool_use_id,
                            "tool_use_id_short": compact_text(tool_use_id, 16),
                            "name": tool_name,
                            "input": block.get("input"),
                        }));
                    }
                }
                render_state
                    .tool_lifecycle
                    .observe_stream_tool_start(root, event, now);
            }
            Some("content_block_delta") => {
                if event
                    .get("delta")
                    .and_then(|delta| delta.get("type"))
                    .and_then(Value::as_str)
                    == Some("input_json_delta")
                {
                    render_state
                        .tool_lifecycle
                        .observe_stream_tool_input_delta(event);
                }
            }
            Some("content_block_stop") => {
                if let Some((tool_id, name, input)) = render_state
                    .tool_lifecycle
                    .observe_stream_tool_block_stop(event)
                {
                    if let Some(focus) = summarize_tool_call_input(&name, input.as_ref()) {
                        bundle
                            .timeline_lines
                            .push(format!("tool_input_finalized | {name} | {focus}"));
                    }
                    bundle.machine_records.push(json!({
                        "type": "tool_input_finalized",
                        "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                        "tool_use_id": tool_id,
                        "name": name,
                        "input": input,
                    }));
                }
            }
            _ => {}
        }
    }

    if root_type == Some("result") {
        return bundle;
    }

    if root_type == Some("assistant") {
        render_state
            .tool_lifecycle
            .observe_assistant_tool_uses(root, now);
        if let Some(parent_tool_use_id) = root.get("parent_tool_use_id").and_then(Value::as_str) {
            let preview_full = extract_text_blocks(
                root.get("message")
                    .and_then(|message| message.get("content"))
                    .or_else(|| root.get("content")),
            );
            let preview = preview_full
                .as_deref()
                .map(|text| compact_text(text, 120))
                .unwrap_or_else(|| "working...".to_string());
            let model = root
                .get("message")
                .and_then(|message| message.get("model"))
                .or_else(|| root.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let line = format!(
                "{} | model={} | {}",
                compact_text(parent_tool_use_id, 16),
                model,
                preview
            );
            bundle.subagent_lines.push(line.clone());
            bundle.subagent_updates.push(SubagentUiUpdate {
                tool_use_id: parent_tool_use_id.to_string(),
                status: LiveCallStatus::Running,
                model: Some(model.to_string()),
                preview: Some(preview.clone()),
                summary: None,
                duration_ms: None,
            });
            bundle.machine_records.push(json!({
                "type": "subagent_update",
                "parent_tool_use_id": parent_tool_use_id,
                "model": model,
                "preview": preview,
                "preview_full": preview_full.as_deref(),
            }));
        }
    }

    if root_type != Some("user") {
        return bundle;
    }

    let content = root
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array);
    let Some(content) = content else {
        return bundle;
    };

    let mut root_diff_consumed = false;
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let tool_use_id = item
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let is_error = item
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = if is_error { "error" } else { "ok" };
        let content_text = item
            .get("content")
            .map(tool_result_content_as_text)
            .unwrap_or_default();
        let exit_code = extract_exit_code(&content_text);
        let result_full = {
            let cleaned = sanitize_summary_text(&content_text);
            if cleaned.trim().is_empty() {
                None
            } else {
                Some(cleaned)
            }
        };
        let excerpt = result_full.as_deref().map(|text| compact_text(text, 140));

        let completed = render_state
            .tool_lifecycle
            .complete_tool_call(tool_use_id, now);
        let actor = completed
            .as_ref()
            .map(|tool| tool.actor.clone())
            .unwrap_or_else(|| actor_label(root));
        let name = completed
            .as_ref()
            .map(|tool| tool.name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let duration_ms = completed.as_ref().map(|tool| tool.duration_ms);
        let input_value = completed.as_ref().and_then(|tool| tool.input.as_ref());

        if !root_diff_consumed {
            if let Some(diff) = render_tool_result_diff(
                root.get("tool_use_result"),
                input_value,
                &name,
                tool_use_id,
            ) {
                root_diff_consumed = true;
                if let Some(file_path) = diff.file_path.as_deref() {
                    bundle.timeline_lines.push(format!(
                        "diff_captured | {} | hunks={} | lines={}",
                        compact_path_tail(file_path, 72),
                        diff.hunk_count,
                        diff.lines.len()
                    ));
                }
                if diff.truncated {
                    bundle
                        .timeline_lines
                        .push("diff_truncated | large patch clipped for live UI".to_string());
                }
                bundle.machine_records.push(json!({
                    "type": "code_diff",
                    "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                    "tool_use_id": tool_use_id,
                    "name": name.clone(),
                    "file_path": diff.file_path.as_deref(),
                    "hunk_count": diff.hunk_count,
                    "line_count": diff.lines.len(),
                    "truncated": diff.truncated,
                }));
                bundle.diff_lines.extend(diff.lines);
            }
        }

        let phase = phase_for_tool(&name, input_value);
        if let Some(phase) = phase {
            if let Some(changed) = render_state.phase_tracker.transition_to(phase) {
                bundle.activities.push(format!(
                    "{} | to={}",
                    activity_head(&actor, "phase_change"),
                    changed.as_str()
                ));
                bundle
                    .timeline_lines
                    .push(format!("phase_started | {}", changed.as_str()));
                bundle.machine_records.push(json!({
                    "type": "phase_started",
                    "actor": actor,
                    "phase": changed.as_str(),
                    "trigger_tool_use_id": tool_use_id,
                    "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                }));
            }

            if phase == RunPhase::Implement {
                if let Some(cause) = render_state.phase_tracker.take_validation_error() {
                    bundle.activities.push(format!(
                        "{} | cause={}",
                        activity_head(&actor, "fix_cycle_started"),
                        compact_text(&cause, 120)
                    ));
                    bundle
                        .timeline_lines
                        .push(format!("fix_applied | cause={}", compact_text(&cause, 80)));
                    bundle.output_lines.push(format!(
                        "Decision | Validation failed with '{}' so applying focused code fixes before retrying.",
                        compact_text(&cause, 80)
                    ));
                    bundle.machine_records.push(json!({
                        "type": "fix_applied",
                        "actor": actor,
                        "cause": cause,
                        "trigger_tool_use_id": tool_use_id,
                        "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                    }));
                    bundle.machine_records.push(json!({
                        "type": "decision_log",
                        "actor": actor,
                        "decision": "switch_to_implement_for_fix",
                        "because": compact_text(&cause, 120),
                        "trigger_tool_use_id": tool_use_id,
                        "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                    }));
                }
            }
        }

        let validation_key = validation_key_for_tool(&name, input_value);
        let mut validation_attempt = None;
        if let Some(key) = validation_key.as_deref() {
            let attempt = render_state.phase_tracker.next_validation_attempt(key);
            validation_attempt = Some(attempt);
            bundle.activities.push(format!(
                "{} | check={key} | attempt={attempt}",
                activity_head(&actor, "validation_attempt")
            ));
            bundle
                .timeline_lines
                .push(format!("validation_started | {key} | attempt={attempt}"));
            if attempt > 1 {
                bundle.machine_records.push(json!({
                    "type": "retry_started",
                    "actor": actor,
                    "check": key,
                    "attempt": attempt,
                    "trigger_tool_use_id": tool_use_id,
                    "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                }));
            }
        }

        let mut parts = vec![
            activity_head(&actor, "tool_done"),
            format!("id={}", compact_text(tool_use_id, 16)),
            format!("name={name}"),
            format!("status={status}"),
        ];
        if let Some(duration_ms) = duration_ms {
            parts.push(format!("duration_ms={duration_ms}"));
        }
        if let Some(attempt) = validation_attempt {
            parts.push(format!("attempt={attempt}"));
        }
        if let Some(exit_code) = exit_code {
            parts.push(format!("exit_code={exit_code}"));
        }
        let tool_input_summary = summarize_tool_call_input(&name, input_value);
        if let Some(summary) = tool_input_summary.as_deref() {
            parts.push(format!("input={summary}"));
        } else if let Some(input) = input_value.and_then(|value| compact_json(value, 140)) {
            parts.push(format!("input={input}"));
        }
        if let Some(excerpt) = excerpt.as_deref() {
            parts.push(format!("result={excerpt}"));
        }
        bundle.tool_updates.push(ToolCallUiUpdate {
            tool_use_id: tool_use_id.to_string(),
            actor: actor.clone(),
            name: name.clone(),
            status: if is_error {
                LiveCallStatus::Error
            } else {
                LiveCallStatus::Ok
            },
            duration_ms,
            detail: tool_input_summary.clone(),
        });
        bundle.activities.push(parts.join(" | "));
        if is_error || duration_ms.map(|value| value >= 2000).unwrap_or(false) {
            let mut timeline = format!("tool_finished | {name} | status={status}");
            if let Some(value) = duration_ms {
                timeline.push_str(&format!(" | {value}ms"));
            }
            if let Some(summary) = tool_input_summary.as_deref() {
                timeline.push_str(" | ");
                timeline.push_str(summary);
            }
            timeline.push_str(&format!(" | {}", compact_text(tool_use_id, 16)));
            bundle.timeline_lines.push(timeline);
        }
        bundle.machine_records.push(json!({
            "type": "tool_finished",
            "actor": actor.clone(),
            "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
            "tool_use_id": tool_use_id,
            "tool_use_id_short": compact_text(tool_use_id, 16),
            "name": name.clone(),
            "status": status,
            "phase": phase.map(|value| value.as_str()),
            "duration_ms": duration_ms,
            "exit_code": exit_code,
            "validation_check": validation_key.clone(),
            "validation_attempt": validation_attempt,
            "input": input_value,
            "result_excerpt": excerpt.as_deref(),
            "result_full": result_full.as_deref(),
        }));
        if name == "Agent" {
            let subagent_summary = excerpt
                .as_deref()
                .map(|text| compact_text(text, 120))
                .unwrap_or_else(|| "no textual summary".to_string());
            bundle.subagent_lines.push(format!(
                "main_agent_used_subagent | {} | {}",
                compact_text(tool_use_id, 16),
                subagent_summary
            ));
            bundle.machine_records.push(json!({
                "type": "subagent_result_used",
                "actor": actor,
                "tool_use_id": tool_use_id,
                "summary": subagent_summary.clone(),
            }));
            bundle.subagent_updates.push(SubagentUiUpdate {
                tool_use_id: tool_use_id.to_string(),
                status: if is_error {
                    LiveCallStatus::Error
                } else {
                    LiveCallStatus::Ok
                },
                model: None,
                preview: None,
                summary: Some(subagent_summary),
                duration_ms,
            });
        }

        if name == "Edit" || name == "Write" {
            if let Some(file_path) = tool_file_path_from_input(input_value) {
                bundle.timeline_lines.push(format!(
                    "file_changed | {}",
                    compact_path_tail(file_path, 72)
                ));
            }
        }

        if validation_attempt.is_some() {
            if is_error {
                render_state
                    .phase_tracker
                    .remember_validation_error(excerpt.as_deref());
            } else {
                render_state.phase_tracker.clear_validation_error();
            }
            if let Some(key) = validation_key.as_deref() {
                let mut result_parts = vec![
                    activity_head(&actor, "validation_result"),
                    format!("check={key}"),
                    format!("status={status}"),
                ];
                if let Some(exit_code) = exit_code {
                    result_parts.push(format!("exit_code={exit_code}"));
                }
                if let Some(excerpt) = excerpt.as_deref() {
                    result_parts.push(format!("reason={excerpt}"));
                }
                bundle.activities.push(result_parts.join(" | "));
                bundle.timeline_lines.push(format!(
                    "validation_result | {key} | attempt={} | {status}{}",
                    validation_attempt.unwrap_or(1),
                    exit_code
                        .map(|code| format!(" | exit {code}"))
                        .unwrap_or_default()
                ));
                bundle.machine_records.push(json!({
                    "type": if status == "ok" { "validation_passed" } else { "validation_failed" },
                    "actor": actor,
                    "parent_tool_use_id": root.get("parent_tool_use_id").and_then(Value::as_str),
                    "tool_use_id": tool_use_id,
                    "check": key,
                    "attempt": validation_attempt.unwrap_or(1),
                    "status": status,
                    "exit_code": exit_code,
                    "reason": excerpt.as_deref(),
                    "reason_full": result_full.as_deref(),
                }));
            }
        }
    }

    bundle
}

fn tool_result_content_as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                Value::Object(map) => map
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| serde_json::to_string(item).unwrap_or_default()),
                _ => serde_json::to_string(item).unwrap_or_default(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn render_tool_result_diff(
    tool_use_result: Option<&Value>,
    tool_input: Option<&Value>,
    tool_name: &str,
    tool_use_id: &str,
) -> Option<RenderedDiff> {
    let tool_use_result = tool_use_result?;
    let object = tool_use_result.as_object()?;
    let file_path = tool_result_file_path(Some(tool_use_result))
        .or_else(|| tool_file_path_from_input(tool_input))
        .map(ToOwned::to_owned);
    let file_label = file_path
        .as_deref()
        .map(|path| compact_path_tail(path, 110))
        .unwrap_or_else(|| "<unknown-file>".to_string());

    if let Some(hunks) = object.get("structuredPatch").and_then(Value::as_array) {
        if !hunks.is_empty() {
            let mut lines = vec![format!(
                "diff -- {file_label} | {tool_name} {}",
                compact_text(tool_use_id, 12)
            )];
            let mut hunk_count = 0_usize;
            for hunk in hunks {
                let old_start = hunk.get("oldStart").and_then(Value::as_u64).unwrap_or(0);
                let old_lines = hunk.get("oldLines").and_then(Value::as_u64).unwrap_or(0);
                let new_start = hunk.get("newStart").and_then(Value::as_u64).unwrap_or(0);
                let new_lines = hunk.get("newLines").and_then(Value::as_u64).unwrap_or(0);
                lines.push(format!(
                    "@@ -{},{} +{},{} @@",
                    old_start, old_lines, new_start, new_lines
                ));
                hunk_count = hunk_count.saturating_add(1);
                if let Some(raw_lines) = hunk.get("lines").and_then(Value::as_array) {
                    for line in raw_lines {
                        if let Some(line) = line.as_str() {
                            lines.push(sanitize_summary_text(line));
                        }
                    }
                }
            }
            let truncated = truncate_diff_lines_for_live_ui(&mut lines);
            return Some(RenderedDiff {
                file_path,
                hunk_count,
                lines,
                truncated,
            });
        }
    }

    let old_string = object.get("oldString").and_then(Value::as_str);
    let new_string = object.get("newString").and_then(Value::as_str);
    let (Some(old_string), Some(new_string)) = (old_string, new_string) else {
        return None;
    };
    if old_string == new_string {
        return None;
    }

    let mut lines = vec![format!(
        "diff -- {file_label} | {tool_name} {}",
        compact_text(tool_use_id, 12)
    )];
    lines.extend(render_old_new_diff(old_string, new_string));
    let truncated = truncate_diff_lines_for_live_ui(&mut lines);
    Some(RenderedDiff {
        file_path,
        hunk_count: 1,
        lines,
        truncated,
    })
}

fn tool_result_file_path(value: Option<&Value>) -> Option<&str> {
    let value = value?;
    value
        .get("filePath")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("file")
                .and_then(|file| file.get("filePath"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("path").and_then(Value::as_str))
        .or_else(|| value.get("target_file").and_then(Value::as_str))
        .or_else(|| value.get("originalFilePath").and_then(Value::as_str))
}

fn render_old_new_diff(old: &str, new: &str) -> Vec<String> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines == new_lines {
        return vec![
            "@@".to_string(),
            "-[content changed]".to_string(),
            "+[content changed]".to_string(),
        ];
    }

    let mut prefix = 0_usize;
    let min_len = old_lines.len().min(new_lines.len());
    while prefix < min_len && old_lines[prefix] == new_lines[prefix] {
        prefix += 1;
    }

    let mut old_suffix = old_lines.len();
    let mut new_suffix = new_lines.len();
    while old_suffix > prefix
        && new_suffix > prefix
        && old_lines[old_suffix - 1] == new_lines[new_suffix - 1]
    {
        old_suffix -= 1;
        new_suffix -= 1;
    }

    let old_changed = &old_lines[prefix..old_suffix];
    let new_changed = &new_lines[prefix..new_suffix];
    let old_start = prefix.saturating_add(1);
    let new_start = prefix.saturating_add(1);

    let mut lines = vec![format!(
        "@@ -{},{} +{},{} @@",
        old_start,
        old_changed.len(),
        new_start,
        new_changed.len()
    )];

    for line in old_changed {
        lines.push(format!("-{}", sanitize_summary_text(line)));
    }
    for line in new_changed {
        lines.push(format!("+{}", sanitize_summary_text(line)));
    }

    if lines.len() == 1 {
        // Covers newline-only or whitespace-only edge cases after line splitting.
        lines.push("-[content changed]".to_string());
        lines.push("+[content changed]".to_string());
    }

    lines
}

fn truncate_diff_lines_for_live_ui(lines: &mut Vec<String>) -> bool {
    if lines.len() <= MAX_DIFF_LINES_PER_EVENT {
        return false;
    }
    let omitted = lines.len().saturating_sub(MAX_DIFF_LINES_PER_EVENT);
    lines.truncate(MAX_DIFF_LINES_PER_EVENT);
    lines.push(format!(
        "... ({omitted} additional diff lines omitted in live view)"
    ));
    true
}

fn extract_exit_code(text: &str) -> Option<i32> {
    for line in text.lines() {
        let trimmed = line.trim();
        for prefix in ["Exit code ", "Error: Exit code "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(value) = rest.split_whitespace().next() {
                    if let Ok(code) = value.parse::<i32>() {
                        return Some(code);
                    }
                }
            }
        }
    }
    None
}

fn phase_for_tool(name: &str, input: Option<&Value>) -> Option<RunPhase> {
    match name {
        "Edit" | "Write" => return Some(RunPhase::Implement),
        "Read" | "Agent" | "Glob" | "Grep" => return Some(RunPhase::Discover),
        _ => {}
    }

    if name != "Bash" {
        return None;
    }

    let command = bash_command_from_input(input)?.to_lowercase();
    if command.contains("git add")
        || command.contains("git commit")
        || command.contains("bd close")
        || command.contains("bd list --status open")
    {
        return Some(RunPhase::Finalize);
    }

    if command.contains("cargo ")
        || command.contains("pytest")
        || command.contains("clippy")
        || command.contains("fmt --check")
        || command.contains("cargo test")
    {
        return Some(RunPhase::Validate);
    }

    Some(RunPhase::Discover)
}

fn validation_key_for_tool(name: &str, input: Option<&Value>) -> Option<String> {
    if name != "Bash" {
        return None;
    }

    let command = bash_command_from_input(input)?.to_lowercase();
    if command.contains("cargo fmt --all --check")
        && command.contains("cargo clippy")
        && command.contains("cargo test")
    {
        return Some("full_validation".to_string());
    }
    if command.contains("cargo clippy") {
        return Some("clippy".to_string());
    }
    if command.contains("cargo test") {
        return Some("tests".to_string());
    }
    if command.contains("cargo fmt --all --check") || command.contains("cargo fmt") {
        return Some("format".to_string());
    }
    if command.contains("cargo build") {
        return Some("build".to_string());
    }

    None
}

fn bash_command_from_input(input: Option<&Value>) -> Option<&str> {
    input?
        .as_object()
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
}

fn tool_file_path_from_input(input: Option<&Value>) -> Option<&str> {
    input?
        .as_object()
        .and_then(|object| object.get("file_path"))
        .and_then(Value::as_str)
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let cleaned = sanitize_summary_text(value);
    let flattened = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if FULL_ACTIVITY_TEXT.load(Ordering::Relaxed) || flattened.chars().count() <= max_chars {
        flattened
    } else {
        let keep = max_chars.saturating_sub(3);
        let mut shortened: String = flattened.chars().take(keep).collect();
        shortened.push_str("...");
        shortened
    }
}

fn sanitize_summary_text(value: &str) -> String {
    strip_ansi_sequences(value)
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t' | '\r'))
        .collect()
}

fn strip_ansi_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut previous_was_escape = false;
                for c in chars.by_ref() {
                    if c == '\u{7}' {
                        break;
                    }
                    if previous_was_escape && c == '\\' {
                        break;
                    }
                    previous_was_escape = c == '\u{1b}';
                }
            }
            _ => {}
        }
    }

    output
}

fn compact_json(value: &Value, max_chars: usize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|raw| compact_text(&raw, max_chars))
}

fn event_tool_name(event: Option<&Value>) -> Option<&str> {
    event
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .and_then(|value| value.get("tool_name"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .and_then(|value| value.get("tool"))
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str)
        })
}

fn actor_label(root: &Value) -> String {
    if let Some(parent) = root.get("parent_tool_use_id").and_then(Value::as_str) {
        format!("subagent({})", compact_text(parent, 12))
    } else {
        "claude".to_string()
    }
}

fn usage_summary(usage: Option<&Value>) -> Option<String> {
    let usage = usage?.as_object()?;
    let mut parts = Vec::new();

    if let Some(value) = usage.get("input_tokens").and_then(Value::as_u64) {
        parts.push(format!("in={value}"));
    }
    if let Some(value) = usage.get("output_tokens").and_then(Value::as_u64) {
        parts.push(format!("out={value}"));
    }
    if let Some(value) = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
    {
        parts.push(format!("cache_write={value}"));
    }
    if let Some(value) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
        parts.push(format!("cache_read={value}"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn summarize_tool_input(input: Option<&Value>) -> Option<String> {
    let input = input?;
    if let Some(object) = input.as_object() {
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let preview_keys = keys.into_iter().take(6).collect::<Vec<_>>();
        let keys_text = if preview_keys.is_empty() {
            "keys=[]".to_string()
        } else {
            format!("keys=[{}]", preview_keys.join(","))
        };
        if let Some(payload) = compact_json(input, 120) {
            return Some(format!("{keys_text} payload={payload}"));
        }
        return Some(keys_text);
    }

    compact_json(input, 120).map(|payload| format!("payload={payload}"))
}

fn summarize_tool_call_input(name: &str, input: Option<&Value>) -> Option<String> {
    let object = input?.as_object()?;

    match name {
        "Bash" => object
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("cmd={}", compact_text(command, 90))),
        "Read" | "Write" | "Edit" => object
            .get("file_path")
            .and_then(Value::as_str)
            .map(|path| format!("file={}", compact_path_tail(path, 90))),
        "Glob" => object
            .get("pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("pattern={}", compact_text(pattern, 90))),
        "Grep" => {
            let pattern = object.get("pattern").and_then(Value::as_str);
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .map(|value| compact_path_tail(value, 48));
            match (pattern, path) {
                (Some(pattern), Some(path)) => {
                    Some(format!("grep={} in {}", compact_text(pattern, 48), path))
                }
                (Some(pattern), None) => Some(format!("grep={}", compact_text(pattern, 90))),
                (None, Some(path)) => Some(format!("path={path}")),
                (None, None) => None,
            }
        }
        "ToolSearch" => object
            .get("query")
            .and_then(Value::as_str)
            .map(|query| format!("query={}", compact_text(query, 90))),
        _ => summarize_tool_input(input),
    }
}

fn compact_path_tail(path: &str, max_chars: usize) -> String {
    if FULL_ACTIVITY_TEXT.load(Ordering::Relaxed) {
        return path.to_string();
    }

    let total = path.chars().count();
    if total <= max_chars {
        return path.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut tail = path.chars().rev().take(keep).collect::<Vec<_>>();
    tail.reverse();
    format!("...{}", tail.into_iter().collect::<String>())
}

fn summarize_event_content(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return Some(compact_text(text, 100));
    }
    compact_json(content, 100)
}

fn spinner_label_for_event(root: &Value, event: Option<&Value>) -> Option<String> {
    let event_type = event
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)?;
    let is_subagent = root.get("parent_tool_use_id").is_some();

    match event_type {
        "message_start" | "message_delta" | "content_block_delta" => Some(if is_subagent {
            "Subagent thinking".to_string()
        } else {
            "Thinking".to_string()
        }),
        "content_block_start" => {
            let block = event.and_then(|value| value.get("content_block"));
            let block_type = block
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str);
            match block_type {
                Some("thinking") => Some(if is_subagent {
                    "Subagent thinking".to_string()
                } else {
                    "Thinking".to_string()
                }),
                Some("tool_use") => {
                    let tool_name = block
                        .and_then(|value| value.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let detail = summarize_tool_call_input(
                        tool_name,
                        block.and_then(|value| value.get("input")),
                    );
                    match detail {
                        Some(detail) => Some(format!("Starting {tool_name}: {detail}")),
                        None => Some(format!("Starting tool: {tool_name}")),
                    }
                }
                _ => None,
            }
        }
        "tool_use" | "tool_call" => {
            let tool_name = event_tool_name(event).unwrap_or("unknown");
            let detail =
                summarize_tool_call_input(tool_name, event.and_then(|value| value.get("input")));
            match detail {
                Some(detail) => Some(format!("Running {tool_name}: {detail}")),
                None => Some(format!("Running tool: {tool_name}")),
            }
        }
        "tool_result" => Some("Processing tool result".to_string()),
        _ => None,
    }
}

fn should_stop_spinner(root: &Value, event: Option<&Value>) -> bool {
    matches!(
        event
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str),
        Some("message_stop" | "content_block_stop" | "error")
    ) || matches!(
        root.get("type").and_then(Value::as_str),
        Some("assistant" | "result" | "rate_limit_event")
    )
}

fn activity_for_event(root: &Value, event: Option<&Value>) -> Option<String> {
    let actor = actor_label(root);
    let root_type = root.get("type").and_then(Value::as_str);
    let event_type = event
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);

    match event_type {
        Some("message_start") => {
            let model = event
                .and_then(|value| value.get("message"))
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .or_else(|| root.get("model").and_then(Value::as_str))
                .unwrap_or("unknown");
            let usage = usage_summary(
                event
                    .and_then(|value| value.get("message"))
                    .and_then(|message| message.get("usage")),
            );
            let mut parts = vec![
                activity_head(&actor, "message_start"),
                format!("model={model}"),
            ];
            if let Some(summary) = usage {
                parts.push(format!("usage({summary})"));
            }
            Some(parts.join(" | "))
        }
        Some("message_delta") => {
            let stop_reason = event
                .and_then(|value| value.get("delta"))
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str);
            let usage = usage_summary(event.and_then(|value| value.get("usage")));
            if stop_reason.is_none() && usage.is_none() {
                return None;
            }
            let mut parts = vec![activity_head(&actor, "message_delta")];
            if let Some(reason) = stop_reason {
                parts.push(format!("stop_reason={reason}"));
            }
            if let Some(summary) = usage {
                parts.push(format!("usage({summary})"));
            }
            Some(parts.join(" | "))
        }
        Some("message_stop") => Some(activity_head(&actor, "message_stop")),
        Some("content_block_start") => {
            let block = event.and_then(|value| value.get("content_block"));
            let block_type = block
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let index = event
                .and_then(|value| value.get("index"))
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string());

            if block_type == "tool_use" {
                let tool_name = block
                    .and_then(|content| content.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let tool_use_id = block
                    .and_then(|content| content.get("id"))
                    .and_then(Value::as_str)
                    .map(|value| compact_text(value, 16))
                    .unwrap_or_else(|| "unknown".to_string());
                let mut parts = vec![
                    activity_head(&actor, "tool_start"),
                    format!("index={index}"),
                    format!("name={tool_name}"),
                    format!("id={tool_use_id}"),
                ];
                if let Some(input) = summarize_tool_call_input(
                    tool_name,
                    block.and_then(|content| content.get("input")),
                ) {
                    parts.push(input);
                }
                return Some(parts.join(" | "));
            }

            Some(format!(
                "{} | index={index} | type={block_type}",
                activity_head(&actor, "block_start")
            ))
        }
        Some("content_block_stop") => Some(format!(
            "{} | index={}",
            activity_head(&actor, "block_stop"),
            event
                .and_then(|value| value.get("index"))
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string())
        )),
        Some("tool_use") | Some("tool_call") => {
            let tool_name = event_tool_name(event).unwrap_or("unknown");
            let input = summarize_tool_call_input(
                tool_name,
                event.and_then(|value| value.get("input")).or_else(|| {
                    event
                        .and_then(|value| value.get("tool"))
                        .and_then(|tool| tool.get("input"))
                }),
            );
            let mut parts = vec![
                activity_head(&actor, "tool_call"),
                format!("name={tool_name}"),
            ];
            if let Some(summary) = input {
                parts.push(summary);
            }
            Some(parts.join(" | "))
        }
        Some("tool_result") => {
            let tool_use_id = event
                .and_then(|value| value.get("tool_use_id"))
                .and_then(Value::as_str)
                .map(|value| compact_text(value, 16))
                .unwrap_or_else(|| "unknown".to_string());
            let is_error = event
                .and_then(|value| value.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if is_error { "error" } else { "ok" };
            let mut parts = vec![
                activity_head(&actor, "tool_result"),
                format!("id={tool_use_id}"),
                format!("status={status}"),
            ];
            if let Some(content) = summarize_event_content(
                event
                    .and_then(|value| value.get("content"))
                    .or_else(|| event.and_then(|value| value.get("result"))),
            ) {
                parts.push(format!("content={content}"));
            }
            Some(parts.join(" | "))
        }
        Some("error") => Some(format!(
            "{} | {}",
            activity_head(&actor, "error"),
            event
                .and_then(|value| value.get("error"))
                .and_then(Value::as_str)
                .or_else(|| {
                    event
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("unknown")
        )),
        _ if root_type == Some("assistant") => {
            let model = root
                .get("message")
                .and_then(|message| message.get("model"))
                .or_else(|| root.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let usage = usage_summary(
                root.get("message")
                    .and_then(|message| message.get("usage"))
                    .or_else(|| root.get("usage")),
            );
            let preview = extract_text_blocks(
                root.get("message")
                    .and_then(|message| message.get("content"))
                    .or_else(|| root.get("content")),
            )
            .map(|text| compact_text(&text, 110));
            let mut parts = vec![activity_head(&actor, "assistant"), format!("model={model}")];
            if let Some(summary) = usage {
                parts.push(format!("usage({summary})"));
            }
            if let Some(text) = preview {
                parts.push(format!("preview={text}"));
            }
            Some(parts.join(" | "))
        }
        _ if root_type == Some("result") => {
            let subtype = root
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let turns = root.get("num_turns").and_then(Value::as_u64);
            let duration_ms = root.get("duration_ms").and_then(Value::as_u64);
            let total_cost = root.get("total_cost_usd").and_then(Value::as_f64);
            let mut parts = vec![
                activity_head(&actor, "result"),
                format!("subtype={subtype}"),
            ];
            if let Some(turns) = turns {
                parts.push(format!("turns={turns}"));
            }
            if let Some(duration) = duration_ms {
                parts.push(format!("duration_ms={duration}"));
            }
            if let Some(cost) = total_cost {
                parts.push(format!("cost_usd={cost:.6}"));
            }
            if let Some(text) = root.get("result").and_then(Value::as_str) {
                parts.push(format!("summary={}", compact_text(text, 110)));
            }
            Some(parts.join(" | "))
        }
        _ if root_type == Some("rate_limit_event") => {
            let rate_limit = parse_rate_limit_event(root);
            let status = rate_limit
                .status
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            let reason = rate_limit.reason();
            let limit_type = rate_limit
                .limit_type
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            let reset_at = rate_limit
                .reset_at_local()
                .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            Some(format!(
                "{} | status={status} | type={limit_type} | reason={reason} | blocking={} | reset_at={reset_at}",
                activity_head(&actor, "rate_limit_event"),
                rate_limit.is_blocking()
            ))
        }
        _ => None,
    }
}

fn activity_head(actor: &str, event: &str) -> String {
    if actor == "claude" {
        event.to_string()
    } else {
        format!("{actor}: {event}")
    }
}

fn send_tool_log(
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Some(logs) = debug_logs.as_mut() {
        logs.log_semantic_line("tool_note", &message);
    }
    send(ui_tx, UiEvent::Progress(message));
}
