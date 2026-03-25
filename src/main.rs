use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde_json::{json, Value};

mod build_info;
mod capture;
mod claude;
mod cli;
mod init;
mod issues;
mod prompts;
mod run_state;
mod runner;
mod settings;
mod summary;
mod terminal;

use crate::capture::is_transient_error_text;
use crate::claude::ClaudeOutcome;
use crate::cli::{Cli, Paths};
use crate::run_state::{RunState, RunStateGuard};
use crate::settings::{CloseGuardrailMode, RalphConfig, RuntimeSettings};
use crate::terminal::{terminal_input_bytes, EmbeddedTerminal};

const DEFAULT_META_PROMPT: &str = include_str!("../prompts/ralph.md");
const DEFAULT_ISSUE_PROMPT: &str = include_str!("../prompts/issue.md");
const DEFAULT_CLEANUP_PROMPT: &str = include_str!("../prompts/cleanup.md");
const DEFAULT_REPAIR_PROMPT: &str = include_str!("../prompts/repair.md");
const DEFAULT_QUALITY_CHECK_PROMPT: &str = include_str!("../prompts/quality-check.md");
const DEFAULT_CODE_REVIEW_CHECK_PROMPT: &str = include_str!("../prompts/code-review-check.md");
const DEFAULT_VALIDATION_CHECK_PROMPT: &str = include_str!("../prompts/validation-check.md");
static FULL_ACTIVITY_TEXT: AtomicBool = AtomicBool::new(false);
static RUNTIME_SETTINGS: OnceLock<RuntimeSettings> = OnceLock::new();
const MAX_LOG_LINES: usize = 200;
const MAX_OUTPUT_LINES: usize = 800;
const MAX_ACTIVITY_LINES: usize = 1200;
const MAX_DIFF_LINES: usize = 2400;
const MAX_DIFF_LINES_PER_EVENT: usize = 280;
const MAX_LIVE_CALLS: usize = 300;
const MAX_UI_EVENTS_PER_TICK: usize = 200;
const AUTO_SCROLL: u16 = u16::MAX;
const SCROLL_STEP: usize = 3;
const DEFAULT_TUI_MESSAGE: &str =
    "Interactive TUI ready. Toggle columns with 1-4, click panes to change focus, mouse wheel scrolls panels.";
const DEFAULT_ADDITIONAL_ITERATIONS: usize = 5;
const TERMINAL_CLOSE_KEY: &str = "F12";
const BG_MAIN: Color = Color::Rgb(10, 14, 24);
const BG_PANEL: Color = Color::Rgb(18, 24, 38);
const BG_HEADER: Color = Color::Rgb(12, 34, 52);
const BG_FOOTER: Color = Color::Rgb(14, 18, 30);
const FG_MAIN: Color = Color::Rgb(232, 240, 255);
const FG_MUTED: Color = Color::Rgb(168, 182, 211);
const ACCENT_INFO: Color = Color::Rgb(94, 218, 255);
const ACCENT_PROGRESS: Color = Color::Rgb(110, 155, 255);
const ACCENT_ACTIVITY: Color = Color::Rgb(255, 205, 106);
const ACCENT_OUTPUT: Color = Color::Rgb(126, 234, 146);
const ACCENT_WARN: Color = Color::Rgb(255, 175, 102);
const ACCENT_DIFF_ADD: Color = Color::Rgb(112, 228, 132);
const ACCENT_DIFF_REMOVE: Color = Color::Rgb(255, 134, 134);
const ACCENT_DIFF_HUNK: Color = Color::Rgb(124, 194, 255);
const BG_DIFF_ADD: Color = Color::Rgb(18, 46, 28);
const BG_DIFF_REMOVE: Color = Color::Rgb(54, 24, 24);
const BG_DIFF_HUNK: Color = Color::Rgb(20, 36, 58);
const SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiveCallStatus {
    Running,
    Ok,
    Error,
}

#[derive(Clone)]
struct ToolCallUiUpdate {
    tool_use_id: String,
    actor: String,
    name: String,
    status: LiveCallStatus,
    duration_ms: Option<u128>,
    detail: Option<String>,
}

#[derive(Clone)]
struct SubagentUiUpdate {
    tool_use_id: String,
    status: LiveCallStatus,
    model: Option<String>,
    preview: Option<String>,
    summary: Option<String>,
    duration_ms: Option<u128>,
}

#[derive(Clone)]
struct ToolCallUiEntry {
    tool_use_id: String,
    actor: String,
    name: String,
    status: LiveCallStatus,
    detail: Option<String>,
    started_at: Instant,
    duration_ms: Option<u128>,
}

#[derive(Clone)]
struct SubagentUiEntry {
    tool_use_id: String,
    status: LiveCallStatus,
    model: Option<String>,
    preview: Option<String>,
    summary: Option<String>,
    started_at: Instant,
    duration_ms: Option<u128>,
}

struct CleanupGuard;

impl CleanupGuard {
    fn new(_enabled: bool) -> Self {
        Self
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {}
}

struct UiApp {
    status: String,
    issue: String,
    issue_details: String,
    summary: String,
    progress_lines: VecDeque<String>,
    output_lines: VecDeque<String>,
    activity_lines: VecDeque<String>,
    timeline_lines: VecDeque<String>,
    subagent_lines: VecDeque<String>,
    tool_calls: HashMap<String, ToolCallUiEntry>,
    tool_call_order: VecDeque<String>,
    subagent_calls: HashMap<String, SubagentUiEntry>,
    subagent_order: VecDeque<String>,
    diff_lines: VecDeque<String>,
    message: String,
    spinner_label: Option<String>,
    spinner_frame: usize,
    usage: UsageTally,
    total_cost_usd: f64,
    show_output: bool,
    show_diff: bool,
    show_side: bool,
    show_terminal: bool,
    focus_mode: FocusMode,
    embedded_terminal: Option<EmbeddedTerminal>,
    terminal_error: Option<String>,
    terminal_area: Option<Rect>,
    terminal_cwd: Option<PathBuf>,
    graceful_quit_requested: bool,
    progress_scroll: u16,
    issue_details_scroll: u16,
    activity_scroll: u16,
    output_scroll: u16,
    timeline_scroll: u16,
    subagent_scroll: u16,
    diff_scroll: u16,
    awaiting_iteration_extension: bool,
    iteration_input_mode: bool,
    iteration_input_value: String,
    iteration_input_error: Option<String>,
    reflect_available: bool,
    post_run_actions_available: bool,
    should_quit: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusMode {
    Ralph,
    Terminal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomColumn {
    Output,
    Diff,
    Side,
    Terminal,
}

impl UiApp {
    fn new() -> Self {
        Self {
            status: "Starting".to_string(),
            issue: "-".to_string(),
            issue_details: "Issue details will appear here once an iteration begins.".to_string(),
            summary: String::new(),
            progress_lines: VecDeque::new(),
            output_lines: VecDeque::new(),
            activity_lines: VecDeque::new(),
            timeline_lines: VecDeque::new(),
            subagent_lines: VecDeque::new(),
            tool_calls: HashMap::new(),
            tool_call_order: VecDeque::new(),
            subagent_calls: HashMap::new(),
            subagent_order: VecDeque::new(),
            diff_lines: VecDeque::new(),
            message: DEFAULT_TUI_MESSAGE.to_string(),
            spinner_label: None,
            spinner_frame: 0,
            usage: UsageTally::default(),
            total_cost_usd: 0.0,
            show_output: true,
            show_diff: true,
            show_side: true,
            show_terminal: false,
            focus_mode: FocusMode::Ralph,
            embedded_terminal: None,
            terminal_error: None,
            terminal_area: None,
            terminal_cwd: None,
            graceful_quit_requested: false,
            progress_scroll: AUTO_SCROLL,
            issue_details_scroll: 0,
            activity_scroll: AUTO_SCROLL,
            output_scroll: AUTO_SCROLL,
            timeline_scroll: AUTO_SCROLL,
            subagent_scroll: AUTO_SCROLL,
            diff_scroll: AUTO_SCROLL,
            awaiting_iteration_extension: false,
            iteration_input_mode: false,
            iteration_input_value: DEFAULT_ADDITIONAL_ITERATIONS.to_string(),
            iteration_input_error: None,
            reflect_available: false,
            post_run_actions_available: false,
            should_quit: false,
        }
    }

    fn push_progress(&mut self, line: impl Into<String>) {
        push_line(&mut self.progress_lines, line.into(), MAX_LOG_LINES);
    }

    fn push_output(&mut self, line: impl Into<String>) {
        push_line(&mut self.output_lines, line.into(), MAX_OUTPUT_LINES);
    }

    fn append_output_chunk(&mut self, chunk: impl AsRef<str>) {
        append_chunk(&mut self.output_lines, chunk.as_ref(), MAX_OUTPUT_LINES);
    }

    fn push_activity(&mut self, line: impl Into<String>) {
        push_line(&mut self.activity_lines, line.into(), MAX_ACTIVITY_LINES);
    }

    fn push_timeline(&mut self, line: impl Into<String>) {
        push_line(&mut self.timeline_lines, line.into(), MAX_ACTIVITY_LINES);
    }

    fn push_subagent(&mut self, line: impl Into<String>) {
        push_line(&mut self.subagent_lines, line.into(), MAX_ACTIVITY_LINES);
    }

    fn push_diff(&mut self, line: impl Into<String>) {
        push_line(&mut self.diff_lines, line.into(), MAX_DIFF_LINES);
    }

    fn apply_tool_call_update(&mut self, update: ToolCallUiUpdate) {
        let now = Instant::now();
        if !self.tool_calls.contains_key(&update.tool_use_id) {
            self.tool_call_order.push_back(update.tool_use_id.clone());
            while self.tool_call_order.len() > MAX_LIVE_CALLS {
                if let Some(removed) = self.tool_call_order.pop_front() {
                    self.tool_calls.remove(&removed);
                }
            }
        }

        let entry = self
            .tool_calls
            .entry(update.tool_use_id.clone())
            .or_insert_with(|| ToolCallUiEntry {
                tool_use_id: update.tool_use_id.clone(),
                actor: update.actor.clone(),
                name: update.name.clone(),
                status: LiveCallStatus::Running,
                detail: update.detail.clone(),
                started_at: now,
                duration_ms: None,
            });

        entry.actor = update.actor;
        entry.name = update.name;
        if update.detail.is_some() {
            entry.detail = update.detail;
        }
        entry.status = update.status;
        if update.status == LiveCallStatus::Running {
            entry.duration_ms = None;
        } else {
            entry.duration_ms = update
                .duration_ms
                .or_else(|| Some(now.duration_since(entry.started_at).as_millis()));
        }
    }

    fn apply_subagent_update(&mut self, update: SubagentUiUpdate) {
        let now = Instant::now();
        if !self.subagent_calls.contains_key(&update.tool_use_id) {
            self.subagent_order.push_back(update.tool_use_id.clone());
            while self.subagent_order.len() > MAX_LIVE_CALLS {
                if let Some(removed) = self.subagent_order.pop_front() {
                    self.subagent_calls.remove(&removed);
                }
            }
        }

        let entry = self
            .subagent_calls
            .entry(update.tool_use_id.clone())
            .or_insert_with(|| SubagentUiEntry {
                tool_use_id: update.tool_use_id.clone(),
                status: LiveCallStatus::Running,
                model: update.model.clone(),
                preview: update.preview.clone(),
                summary: update.summary.clone(),
                started_at: now,
                duration_ms: None,
            });

        if update.model.is_some() {
            entry.model = update.model;
        }
        if update.preview.is_some() {
            entry.preview = update.preview;
        }
        if update.summary.is_some() {
            entry.summary = update.summary;
        }
        entry.status = update.status;
        if update.status == LiveCallStatus::Running {
            entry.duration_ms = None;
        } else {
            entry.duration_ms = update
                .duration_ms
                .or_else(|| Some(now.duration_since(entry.started_at).as_millis()));
        }
    }

    fn has_running_calls(&self) -> bool {
        self.tool_calls
            .values()
            .any(|entry| entry.status == LiveCallStatus::Running)
            || self
                .subagent_calls
                .values()
                .any(|entry| entry.status == LiveCallStatus::Running)
    }

    fn set_terminal_context(&mut self, project_dir: &Path) {
        self.terminal_cwd = Some(project_dir.to_path_buf());
    }

    fn init_terminal(&mut self, project_dir: &Path, area: Rect) -> Result<()> {
        let terminal = EmbeddedTerminal::spawn(
            project_dir,
            area,
            runtime_settings().terminal_scrollback_lines,
        )?;
        self.embedded_terminal = Some(terminal);
        self.terminal_error = None;
        self.terminal_area = Some(area);
        self.terminal_cwd = Some(project_dir.to_path_buf());
        Ok(())
    }

    fn has_terminal(&self) -> bool {
        self.embedded_terminal.is_some()
    }

    fn terminal_visible(&self) -> bool {
        self.show_terminal
    }

    fn terminal_focused(&self) -> bool {
        self.terminal_visible()
            && self.focus_mode == FocusMode::Terminal
            && self.embedded_terminal.is_some()
    }

    fn focus_ralph(&mut self) {
        self.focus_mode = FocusMode::Ralph;
    }

    fn focus_terminal(&mut self) {
        if self.show_terminal && self.embedded_terminal.is_some() {
            self.focus_mode = FocusMode::Terminal;
        }
    }

    fn visible_bottom_column_count(&self) -> usize {
        usize::from(self.show_output)
            + usize::from(self.show_diff)
            + usize::from(self.show_side)
            + usize::from(self.show_terminal)
    }

    fn column_visible(&self, column: BottomColumn) -> bool {
        match column {
            BottomColumn::Output => self.show_output,
            BottomColumn::Diff => self.show_diff,
            BottomColumn::Side => self.show_side,
            BottomColumn::Terminal => self.show_terminal,
        }
    }

    fn visible_bottom_columns(&self) -> Vec<BottomColumn> {
        let mut columns = Vec::with_capacity(4);
        if self.show_output {
            columns.push(BottomColumn::Output);
        }
        if self.show_diff {
            columns.push(BottomColumn::Diff);
        }
        if self.show_side {
            columns.push(BottomColumn::Side);
        }
        if self.show_terminal {
            columns.push(BottomColumn::Terminal);
        }
        columns
    }

    fn hide_column(&mut self, column: BottomColumn) -> Result<()> {
        if !self.column_visible(column) {
            return Ok(());
        }
        match column {
            BottomColumn::Terminal => self.force_close_terminal(),
            BottomColumn::Output | BottomColumn::Diff | BottomColumn::Side => {
                if self.visible_bottom_column_count() <= 1 {
                    bail!("at least one bottom column must remain visible");
                }
                match column {
                    BottomColumn::Output => self.show_output = false,
                    BottomColumn::Diff => self.show_diff = false,
                    BottomColumn::Side => self.show_side = false,
                    BottomColumn::Terminal => unreachable!(),
                }
                self.focus_mode = FocusMode::Ralph;
            }
        }
        Ok(())
    }

    fn show_column(&mut self, column: BottomColumn) {
        match column {
            BottomColumn::Output => self.show_output = true,
            BottomColumn::Diff => self.show_diff = true,
            BottomColumn::Side => self.show_side = true,
            BottomColumn::Terminal => self.show_terminal = true,
        }
    }

    fn close_terminal(&mut self) {
        self.force_close_terminal();
    }

    fn ensure_terminal(&mut self, area: Rect) -> Result<bool> {
        if self.embedded_terminal.is_some() {
            return Ok(false);
        }
        let Some(cwd) = self.terminal_cwd.clone() else {
            bail!("terminal spawn path is unavailable");
        };
        self.init_terminal(&cwd, area)?;
        Ok(true)
    }

    fn force_close_terminal(&mut self) {
        self.show_terminal = false;
        self.embedded_terminal = None;
        self.terminal_area = None;
        self.focus_mode = FocusMode::Ralph;
        if self.visible_bottom_column_count() == 0 {
            self.show_diff = true;
        }
    }

    fn sync_terminal_size(&mut self, area: Rect) -> Result<()> {
        let Some(terminal) = self.embedded_terminal.as_mut() else {
            return Ok(());
        };
        if self.terminal_area == Some(area) {
            return Ok(());
        }
        terminal.resize(area)?;
        self.terminal_area = Some(area);
        Ok(())
    }

    fn send_terminal_input(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(terminal) = self.embedded_terminal.as_mut() else {
            bail!("embedded terminal is unavailable");
        };
        terminal.write_input(bytes)
    }

    fn scroll_terminal(&mut self, mouse_kind: MouseEventKind) {
        let Some(terminal) = self.embedded_terminal.as_mut() else {
            return;
        };
        match mouse_kind {
            MouseEventKind::ScrollUp => terminal.scroll_scrollback(-3),
            MouseEventKind::ScrollDown => terminal.scroll_scrollback(3),
            _ => {}
        }
    }

    fn poll_terminal_exit(&mut self) -> Result<Option<String>> {
        let Some(terminal) = self.embedded_terminal.as_mut() else {
            return Ok(None);
        };
        let Some(status) = terminal.try_wait()? else {
            return Ok(None);
        };

        self.close_terminal();
        let detail = if let Some(signal) = status.signal() {
            format!("signal {signal}")
        } else {
            format!("exit code {}", status.exit_code())
        };
        Ok(Some(format!("Embedded terminal closed ({detail})")))
    }

    fn message_text(&self) -> String {
        if self.terminal_focused() {
            "Terminal focused. Typing goes to the embedded shell.".to_string()
        } else {
            self.message.clone()
        }
    }

    fn controls_text(&self) -> String {
        if self.iteration_input_mode {
            "digits edit count, Enter add iterations, Esc cancel".to_string()
        } else if self.awaiting_iteration_extension || self.post_run_actions_available {
            "[1] toggle output  [2] toggle diff  [3] toggle activity  [4]/[t] toggle terminal  [Ctrl+T] focus terminal  [n] 1 more iteration  [x] custom amount  [r] run reflection  [q]/[Esc] exit".to_string()
        } else if self.reflect_available {
            "[1] toggle output  [2] toggle diff  [3] toggle activity  [4]/[t] toggle terminal  [Ctrl+T] focus terminal  [r] run reflection  [q]/[Esc] exit".to_string()
        } else if self.terminal_focused() {
            format!(
                "Typing goes to terminal  [{TERMINAL_CLOSE_KEY}] close terminal  click another pane to return focus"
            )
        } else {
            "[1] toggle output  [2] toggle diff  [3] toggle activity  [4]/[t] toggle terminal  [Ctrl+T] focus terminal  [n] 1 more iteration  [x] custom amount  [q]/[Esc] quit now  [Shift+Q] stop after current iteration".to_string()
        }
    }

    fn set_default_message(&mut self) {
        if self.graceful_quit_requested {
            self.message =
                "Graceful stop requested. Ralph will exit after the current iteration.".to_string();
        } else {
            self.message = DEFAULT_TUI_MESSAGE.to_string();
        }
    }

    fn set_iteration_extension_ready(&mut self, total_iterations: usize) {
        self.awaiting_iteration_extension = true;
        self.iteration_input_mode = false;
        self.iteration_input_value = DEFAULT_ADDITIONAL_ITERATIONS.to_string();
        self.iteration_input_error = None;
        self.reflect_available = true;
        self.focus_ralph();
        self.status = "Waiting for iteration input".to_string();
        self.message = format!(
            "Reached the current iteration budget ({total_iterations}). Press n for 1 more, x for a custom amount, r to run reflection, or q/Esc to exit."
        );
    }

    fn open_iteration_input(&mut self) {
        self.iteration_input_mode = true;
        self.iteration_input_value = DEFAULT_ADDITIONAL_ITERATIONS.to_string();
        self.iteration_input_error = None;
        self.focus_ralph();
    }

    fn close_iteration_input(&mut self) {
        self.iteration_input_mode = false;
        self.iteration_input_error = None;
    }

    fn tool_panel_lines(&self, spinner: &str) -> Vec<String> {
        let mut running = Vec::new();
        let mut complete = Vec::new();
        for tool_use_id in &self.tool_call_order {
            let Some(entry) = self.tool_calls.get(tool_use_id) else {
                continue;
            };
            let status = status_label(entry.status);
            let marker = if entry.status == LiveCallStatus::Running {
                spinner
            } else {
                status_marker(entry.status)
            };
            let runtime = runtime_label(entry.status, entry.started_at, entry.duration_ms);
            let mut line = format!(
                "{marker} tool_call | {} | status={status} | runtime={runtime}",
                entry.name
            );
            if let Some(detail) = entry.detail.as_deref() {
                line.push_str(" | ");
                line.push_str(detail);
            }
            line.push_str(" | id=");
            line.push_str(&compact_text(&entry.tool_use_id, 16));
            if entry.status == LiveCallStatus::Running {
                running.push(line);
            } else {
                complete.push(line);
            }
        }

        if running.is_empty() && complete.is_empty() {
            return if self.timeline_lines.is_empty() {
                vec!["No tool calls yet.".to_string()]
            } else {
                self.timeline_lines.iter().cloned().collect()
            };
        }

        complete.extend(running);
        complete
    }

    fn subagent_panel_lines(&self, spinner: &str) -> Vec<String> {
        let mut running = Vec::new();
        let mut complete = Vec::new();
        for tool_use_id in &self.subagent_order {
            let Some(entry) = self.subagent_calls.get(tool_use_id) else {
                continue;
            };
            let status = status_label(entry.status);
            let marker = if entry.status == LiveCallStatus::Running {
                spinner
            } else {
                status_marker(entry.status)
            };
            let runtime = runtime_label(entry.status, entry.started_at, entry.duration_ms);
            let mut line = format!(
                "{marker} subagent_call | id={} | status={status} | runtime={runtime}",
                compact_text(&entry.tool_use_id, 16)
            );
            if let Some(model) = entry.model.as_deref() {
                line.push_str(" | model=");
                line.push_str(model);
            }
            let extra = if entry.status == LiveCallStatus::Running {
                entry.preview.as_deref()
            } else {
                entry.summary.as_deref().or(entry.preview.as_deref())
            };
            if let Some(text) = extra {
                if entry.status == LiveCallStatus::Running {
                    line.push_str(" | preview=");
                } else {
                    line.push_str(" | summary=");
                }
                line.push_str(&compact_text(text, 120));
            }
            if entry.status == LiveCallStatus::Running {
                running.push(line);
            } else {
                complete.push(line);
            }
        }

        if running.is_empty() && complete.is_empty() {
            return if self.subagent_lines.is_empty() {
                vec!["No subagent activity yet.".to_string()]
            } else {
                self.subagent_lines.iter().cloned().collect()
            };
        }

        complete.extend(running);
        complete
    }
}

#[derive(Clone, Copy)]
enum ScrollTarget {
    Progress,
    IssueDetails,
    Activity,
    Output,
    Diff,
    Terminal,
    Timeline,
    Subagent,
}

#[derive(Clone, Copy)]
struct SideLayout {
    activity: Rect,
    timeline: Rect,
    subagent: Rect,
}

#[derive(Clone, Copy)]
struct RunLayout {
    header: Rect,
    progress: Rect,
    issue_details: Rect,
    output: Option<Rect>,
    diff: Option<Rect>,
    side: Option<SideLayout>,
    terminal: Option<Rect>,
    messages: Rect,
    footer: Rect,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageTally {
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
}

impl UsageTally {
    fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_write_tokens == 0
            && self.cache_read_tokens == 0
    }

    fn add_assign(&mut self, other: UsageTally) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
    }

    fn delta_from_previous(previous: UsageTally, current: UsageTally) -> UsageTally {
        UsageTally {
            input_tokens: usage_component_delta(previous.input_tokens, current.input_tokens),
            output_tokens: usage_component_delta(previous.output_tokens, current.output_tokens),
            cache_write_tokens: usage_component_delta(
                previous.cache_write_tokens,
                current.cache_write_tokens,
            ),
            cache_read_tokens: usage_component_delta(
                previous.cache_read_tokens,
                current.cache_read_tokens,
            ),
        }
    }
}

fn usage_component_delta(previous: u64, current: u64) -> u64 {
    if current >= previous {
        current - previous
    } else {
        current
    }
}

fn push_line(lines: &mut VecDeque<String>, line: String, limit: usize) {
    for fragment in split_for_ui(&line) {
        lines.push_back(fragment);
    }
    while lines.len() > limit {
        lines.pop_front();
    }
}

fn append_chunk(lines: &mut VecDeque<String>, chunk: &str, limit: usize) {
    let normalized = chunk.replace('\t', "    ");
    if lines.is_empty() {
        lines.push_back(String::new());
    }

    let mut parts = normalized.split('\n');
    if let Some(first) = parts.next() {
        if let Some(last) = lines.back_mut() {
            last.push_str(first.trim_end_matches('\r'));
        }
    }

    for part in parts {
        lines.push_back(part.trim_end_matches('\r').to_string());
    }

    while lines.len() > limit {
        lines.pop_front();
    }
}

fn split_for_ui(line: &str) -> Vec<String> {
    let normalized = line.replace('\t', "    ");
    let mut parts = Vec::new();
    for part in normalized.lines() {
        parts.push(part.to_string());
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

fn parse_additional_iterations(input: &str) -> Result<usize, &'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a positive number of iterations.");
    }

    let parsed = trimmed.parse::<usize>().map_err(|_| "Enter digits only.")?;
    if parsed == 0 {
        return Err("Iteration count must be at least 1.");
    }

    Ok(parsed)
}

fn is_terminal_focus_escape(key: crossterm::event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5')))
}

fn is_graceful_stop_key(key: crossterm::event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('Q'))
        || (matches!(key.code, KeyCode::Char('q')) && key.modifiers.contains(KeyModifiers::SHIFT))
}

enum UiEvent {
    Status(String),
    Summary(String),
    Issue(String),
    IssueDetails(String),
    UsageDelta(UsageTally),
    CostDelta(f64),
    Progress(String),
    Output(String),
    OutputChunk(String),
    Activity(String),
    Diff(String),
    Timeline(String),
    Subagent(String),
    ToolCall(ToolCallUiUpdate),
    SubagentCall(SubagentUiUpdate),
    Spinner(Option<String>),
    IterationBudgetReached(usize),
    PostRunReflectAvailable,
    Stop(String),
}

enum WorkerControl {
    ExtendIterations(usize),
    Reflect,
    Exit,
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let paths = Paths::from_cwd()?;
    apply_command_mode(&mut cli);
    let config = load_config(&paths)?;
    let settings = resolve_runtime_settings(&mut cli, &config);
    let _ = RUNTIME_SETTINGS.set(settings.clone());
    validate_cli_arguments(&cli, &settings)?;
    let templates = init::PromptTemplates {
        meta: DEFAULT_META_PROMPT,
        issue: DEFAULT_ISSUE_PROMPT,
        cleanup: DEFAULT_CLEANUP_PROMPT,
        repair: DEFAULT_REPAIR_PROMPT,
        quality_check: DEFAULT_QUALITY_CHECK_PROMPT,
        code_review_check: DEFAULT_CODE_REVIEW_CHECK_PROMPT,
        validation_check: DEFAULT_VALIDATION_CHECK_PROMPT,
    };

    if cli.init {
        return init::init_project(&paths, &templates);
    }

    if cli.doctor {
        return init::doctor_project(&paths, &templates);
    }

    if cli.preflight {
        return run_preflight(&paths, &settings, cli.json);
    }

    if cli.upgrade_prompts {
        return init::upgrade_prompts(&paths, &templates);
    }

    if cli.summary {
        if cli.json {
            return summary::print_last_run_summary_json(&paths);
        }
        return summary::print_last_run_summary(&paths);
    }

    run_main_loop(cli, paths, settings)
}

fn apply_command_mode(cli: &mut Cli) {
    settings::apply_command_mode(cli);
}

fn load_config(paths: &Paths) -> Result<RalphConfig> {
    settings::load_config(paths)
}

fn resolve_runtime_settings(cli: &mut Cli, config: &RalphConfig) -> RuntimeSettings {
    settings::resolve_runtime_settings(cli, config)
}

fn runtime_settings() -> &'static RuntimeSettings {
    RUNTIME_SETTINGS.get_or_init(settings::default_runtime_settings)
}

fn run_preflight(paths: &Paths, settings: &RuntimeSettings, as_json: bool) -> Result<()> {
    settings::run_preflight(paths, settings, as_json)
}

fn validate_cli_arguments(cli: &Cli, settings: &RuntimeSettings) -> Result<()> {
    settings::validate_cli_arguments(cli, settings)
}

fn run_main_loop(cli: Cli, paths: Paths, settings: RuntimeSettings) -> Result<()> {
    runner::run_main_loop(cli, paths, settings)
}

struct DebugLogs {
    run_id: String,
    current_iteration: usize,
    current_issue: String,
    semantic_sequence: u64,
    run_dir_path: PathBuf,
    raw_events_path: PathBuf,
    activity_path: PathBuf,
    output_path: PathBuf,
    semantic_path: PathBuf,
    report_path: PathBuf,
    raw_events_file: fs::File,
    activity_file: fs::File,
    output_file: fs::File,
    semantic_file: fs::File,
    report_file: fs::File,
}

impl DebugLogs {
    fn new(paths: &Paths, run_id: &str) -> Result<Self> {
        let run_dir_path = paths.run_logs_dir(run_id);
        fs::create_dir_all(&run_dir_path)
            .context("failed to create per-run debug log directory")?;

        let raw_events_path = run_dir_path.join("claude-events.log");
        let activity_path = run_dir_path.join("claude-activity.log");
        let output_path = run_dir_path.join("claude-output.log");
        let semantic_path = run_dir_path.join("claude-semantic.ndjson");
        let report_path = run_dir_path.join("claude-output.md");

        let raw_events_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&raw_events_path)
            .context("failed to create raw Claude events debug log")?;
        let activity_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&activity_path)
            .context("failed to create Claude activity debug log")?;
        let output_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&output_path)
            .context("failed to create Claude output debug log")?;
        let semantic_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&semantic_path)
            .context("failed to create Claude semantic debug log")?;
        let mut report_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&report_path)
            .context("failed to create Claude markdown output log")?;
        writeln!(report_file, "# Claude Output Report\n")
            .context("failed to initialize Claude markdown output log")?;

        Ok(Self {
            run_id: run_id.to_string(),
            current_iteration: 0,
            current_issue: String::new(),
            semantic_sequence: 0,
            run_dir_path,
            raw_events_path,
            activity_path,
            output_path,
            semantic_path,
            report_path,
            raw_events_file,
            activity_file,
            output_file,
            semantic_file,
            report_file,
        })
    }

    fn set_iteration_context(&mut self, iteration: usize, issue_id: &str) {
        self.current_iteration = iteration;
        self.current_issue = issue_id.to_string();
    }

    fn log_raw_event(&mut self, is_stderr: bool, line: &str) {
        let stream = if is_stderr { "stderr" } else { "stdout" };
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(self.raw_events_file, "[{timestamp}] [{stream}] {line}");
    }

    fn log_activity(&mut self, line: &str) {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(self.activity_file, "[{timestamp}] {line}");
    }

    fn log_output_chunk(&mut self, chunk: &str) {
        let _ = self.output_file.write_all(chunk.as_bytes());
    }

    fn log_semantic_line(&mut self, category: &str, line: &str) {
        let timestamp = Local::now().to_rfc3339();
        let record = json!({
            "timestamp": timestamp,
            "category": category,
            "line": line,
        });
        let _ = writeln!(self.semantic_file, "{record}");
    }

    fn log_semantic_value(&mut self, value: &Value) {
        let timestamp = Local::now().to_rfc3339();
        self.semantic_sequence = self.semantic_sequence.saturating_add(1);
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("event");
        let parent_tool_use_id = value
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let record = json!({
            "timestamp": timestamp,
            "event_id": format!("{}-{:06}", self.run_id, self.semantic_sequence),
            "run_id": self.run_id,
            "iteration": self.current_iteration,
            "issue_id": self.current_issue,
            "event_type": event_type,
            "parent_tool_use_id": parent_tool_use_id,
            "event": value,
        });
        let _ = writeln!(self.semantic_file, "{record}");
    }

    fn log_report_line(&mut self, line: &str) {
        let _ = writeln!(self.report_file, "{line}");
    }
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let normalized = value.replace(['\n', '\r'], " ");
    let mut compact = normalized
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push('…');
    }
    compact
}

fn run_plain_ui(ui_rx: Receiver<UiEvent>) -> Result<()> {
    while let Ok(event) = ui_rx.recv() {
        match event {
            UiEvent::Status(message) => eprintln!("[ralph] {message}"),
            UiEvent::Summary(message) => eprintln!("[ralph] {message}"),
            UiEvent::Issue(issue) => eprintln!("[ralph] Working on {issue}"),
            UiEvent::IssueDetails(_) => {}
            UiEvent::UsageDelta(_) => {}
            UiEvent::CostDelta(cost) => eprintln!("[usage] cost +${cost:.4}"),
            UiEvent::Progress(line) => eprintln!("[tool] {line}"),
            UiEvent::Output(line) => println!("{line}"),
            UiEvent::OutputChunk(chunk) => {
                print!("{chunk}");
                io::stdout().flush().ok();
            }
            UiEvent::Activity(line) => eprintln!("[claude] {line}"),
            UiEvent::Diff(line) => eprintln!("[diff] {line}"),
            UiEvent::Timeline(line) => eprintln!("[timeline] {line}"),
            UiEvent::Subagent(line) => eprintln!("[subagent] {line}"),
            UiEvent::ToolCall(update) => {
                eprintln!(
                    "[tool_call] {} | {} | status={} | runtime={}{}",
                    update.name,
                    compact_text(&update.tool_use_id, 16),
                    status_label(update.status),
                    runtime_label(update.status, Instant::now(), update.duration_ms),
                    update
                        .detail
                        .as_deref()
                        .map(|value| format!(" | {value}"))
                        .unwrap_or_default()
                );
            }
            UiEvent::SubagentCall(update) => {
                let snippet = if update.status == LiveCallStatus::Running {
                    update.preview.as_deref()
                } else {
                    update.summary.as_deref().or(update.preview.as_deref())
                };
                eprintln!(
                    "[subagent_call] {} | status={} | runtime={}{}",
                    compact_text(&update.tool_use_id, 16),
                    status_label(update.status),
                    runtime_label(update.status, Instant::now(), update.duration_ms),
                    snippet
                        .map(|value| format!(" | {}", compact_text(value, 120)))
                        .unwrap_or_default()
                );
            }
            UiEvent::Spinner(Some(label)) => eprintln!("[claude] {label}"),
            UiEvent::Spinner(None) => {}
            UiEvent::IterationBudgetReached(total_iterations) => eprintln!(
                "[ralph] Reached max iterations ({total_iterations}); TUI input required to continue"
            ),
            UiEvent::PostRunReflectAvailable => {
                eprintln!(
                    "[ralph] Reflection and iteration resume are available from the finished TUI state"
                )
            }
            UiEvent::Stop(line) => {
                eprintln!("[ralph] {line}");
                break;
            }
        }
    }
    Ok(())
}

fn run_live_tui(
    ui_rx: Receiver<UiEvent>,
    control_tx: Sender<WorkerControl>,
    graceful_quit: Arc<AtomicBool>,
    project_dir: PathBuf,
) -> Result<()> {
    let mut terminal = init_terminal()?;
    let result = live_tui_loop(
        ui_rx,
        control_tx,
        graceful_quit,
        &mut terminal,
        &project_dir,
    );
    restore_terminal(&mut terminal)?;
    result
}

fn live_tui_loop(
    ui_rx: Receiver<UiEvent>,
    control_tx: Sender<WorkerControl>,
    graceful_quit: Arc<AtomicBool>,
    terminal: &mut DefaultTerminal,
    project_dir: &Path,
) -> Result<()> {
    let mut app = UiApp::new();
    app.set_terminal_context(project_dir);
    let tick_rate = Duration::from_millis(100);
    let mut last_redraw = Instant::now();
    let mut worker_stopped = false;

    loop {
        for _ in 0..MAX_UI_EVENTS_PER_TICK {
            let Ok(event) = ui_rx.try_recv() else {
                break;
            };
            match event {
                UiEvent::Status(message) => app.status = message,
                UiEvent::Summary(message) => app.summary = message,
                UiEvent::Issue(issue) => app.issue = issue,
                UiEvent::IssueDetails(details) => app.issue_details = details,
                UiEvent::UsageDelta(delta) => app.usage.add_assign(delta),
                UiEvent::CostDelta(cost) => app.total_cost_usd += cost,
                UiEvent::Progress(line) => app.push_progress(line),
                UiEvent::Output(line) => app.push_output(line),
                UiEvent::OutputChunk(chunk) => app.append_output_chunk(chunk),
                UiEvent::Activity(line) => app.push_activity(line),
                UiEvent::Diff(line) => app.push_diff(line),
                UiEvent::Timeline(line) => app.push_timeline(line),
                UiEvent::Subagent(line) => app.push_subagent(line),
                UiEvent::ToolCall(update) => app.apply_tool_call_update(update),
                UiEvent::SubagentCall(update) => app.apply_subagent_update(update),
                UiEvent::Spinner(label) => app.spinner_label = label,
                UiEvent::IterationBudgetReached(total_iterations) => {
                    app.post_run_actions_available = false;
                    app.reflect_available = true;
                    app.spinner_label = None;
                    app.set_iteration_extension_ready(total_iterations);
                    app.push_activity(format!(
                        "Reached iteration budget ({total_iterations}); waiting for TUI input"
                    ));
                }
                UiEvent::PostRunReflectAvailable => {
                    app.reflect_available = true;
                    app.post_run_actions_available = true;
                    if app.status == "Finished" {
                        app.message = "Run finished. Press n for 1 more iteration, x for a custom amount, r to run reflection, or q/Esc to exit.".to_string();
                    }
                }
                UiEvent::Stop(line) => {
                    app.reflect_available = false;
                    app.post_run_actions_available = false;
                    app.awaiting_iteration_extension = false;
                    app.iteration_input_mode = false;
                    app.iteration_input_error = None;
                    app.focus_ralph();
                    if line.contains("rate-limited") {
                        app.status = "Rate limited".to_string();
                        app.message = format!(
                            "{line} | Restart with `ralph` after reset; recovery cleanup runs automatically."
                        );
                    } else {
                        app.status = "Finished".to_string();
                        app.message = format!("{line} | Run finished.");
                    }
                    app.spinner_label = None;
                    worker_stopped = true;
                }
            }
        }

        match app.poll_terminal_exit() {
            Ok(Some(message)) => app.push_activity(message),
            Ok(None) => {}
            Err(error) => app.push_activity(format!("Embedded terminal poll failed: {error:#}")),
        }

        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                CEvent::Key(key) => {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if app.iteration_input_mode {
                        match key.code {
                            KeyCode::Esc => app.close_iteration_input(),
                            KeyCode::Enter => {
                                match parse_additional_iterations(&app.iteration_input_value) {
                                    Ok(additional) => {
                                        let _ = control_tx
                                            .send(WorkerControl::ExtendIterations(additional));
                                        let was_waiting_for_budget =
                                            app.awaiting_iteration_extension;
                                        let was_post_run =
                                            app.post_run_actions_available && worker_stopped;
                                        app.awaiting_iteration_extension = false;
                                        app.post_run_actions_available = false;
                                        app.close_iteration_input();
                                        if was_waiting_for_budget {
                                            app.status = format!(
                                                "Resuming with {additional} more iterations"
                                            );
                                            app.push_activity(format!(
                                                "User requested {additional} additional iterations"
                                            ));
                                            app.set_default_message();
                                        } else if was_post_run {
                                            worker_stopped = false;
                                            app.reflect_available = false;
                                            app.status = format!(
                                                "Resuming with {additional} more iterations"
                                            );
                                            app.push_activity(format!(
                                                "User restarted the finished run with {additional} additional iterations"
                                            ));
                                            app.set_default_message();
                                        } else {
                                            app.status =
                                                format!("Queued {additional} more iterations");
                                            app.message = format!(
                                                "Added {additional} more iterations to the current run. Ralph will pick them up before it reaches the budget."
                                            );
                                            app.push_activity(format!(
                                                "Queued {additional} additional iterations for the active run"
                                            ));
                                        }
                                    }
                                    Err(error) => {
                                        app.iteration_input_error = Some(error.to_string())
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                app.iteration_input_value.pop();
                                app.iteration_input_error = None;
                            }
                            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                                app.iteration_input_value.push(ch);
                                app.iteration_input_error = None;
                            }
                            _ => {}
                        }
                    } else if app.terminal_focused() {
                        if is_terminal_focus_escape(key) {
                            app.close_terminal();
                            app.push_activity("Embedded terminal closed by user".to_string());
                        } else if let Some(bytes) = terminal_input_bytes(key) {
                            if let Err(error) = app.send_terminal_input(&bytes) {
                                app.push_activity(format!(
                                    "Embedded terminal input failed: {error:#}"
                                ));
                                app.focus_ralph();
                            }
                        }
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
                    {
                        if app.has_terminal() {
                            app.focus_terminal();
                        } else {
                            let area: Rect = terminal.size()?.into();
                            app.show_column(BottomColumn::Terminal);
                            let layout = run_layout(area, &app);
                            match layout.terminal {
                                Some(terminal_area) => match app.ensure_terminal(terminal_area) {
                                    Ok(_) => app.focus_terminal(),
                                    Err(error) => {
                                        app.show_terminal = false;
                                        app.push_activity(format!(
                                            "Embedded terminal unavailable: {error:#}"
                                        ));
                                    }
                                },
                                None => app.show_terminal = false,
                            }
                        }
                    } else if matches!(key.code, KeyCode::Char('1')) {
                        if app.show_output {
                            if let Err(error) = app.hide_column(BottomColumn::Output) {
                                app.push_activity(error.to_string());
                            }
                        } else {
                            app.show_column(BottomColumn::Output);
                        }
                    } else if matches!(key.code, KeyCode::Char('2')) {
                        if app.show_diff {
                            if let Err(error) = app.hide_column(BottomColumn::Diff) {
                                app.push_activity(error.to_string());
                            }
                        } else {
                            app.show_column(BottomColumn::Diff);
                        }
                    } else if matches!(key.code, KeyCode::Char('3')) {
                        if app.show_side {
                            if let Err(error) = app.hide_column(BottomColumn::Side) {
                                app.push_activity(error.to_string());
                            }
                        } else {
                            app.show_column(BottomColumn::Side);
                        }
                    } else if matches!(
                        key.code,
                        KeyCode::Char('4') | KeyCode::Char('t') | KeyCode::Char('T')
                    ) {
                        if app.terminal_visible() {
                            app.close_terminal();
                        } else {
                            let area: Rect = terminal.size()?.into();
                            app.show_column(BottomColumn::Terminal);
                            let layout = run_layout(area, &app);
                            match layout.terminal {
                                Some(terminal_area) => match app.ensure_terminal(terminal_area) {
                                    Ok(created) => {
                                        app.focus_terminal();
                                        if created {
                                            app.push_activity(
                                                "Embedded terminal started".to_string(),
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        app.show_terminal = false;
                                        app.push_activity(format!(
                                            "Embedded terminal unavailable: {}",
                                            compact_text(&error.to_string(), 160)
                                        ));
                                    }
                                },
                                None => {
                                    app.show_terminal = false;
                                }
                            }
                        }
                    } else if app.awaiting_iteration_extension {
                        match key.code {
                            KeyCode::Char('n') => {
                                let _ = control_tx.send(WorkerControl::ExtendIterations(1));
                                app.awaiting_iteration_extension = false;
                                app.reflect_available = false;
                                app.status = "Resuming with 1 more iteration".to_string();
                                app.push_activity(
                                    "User requested 1 additional iteration".to_string(),
                                );
                                app.set_default_message();
                            }
                            KeyCode::Char('x') | KeyCode::Char('X') => app.open_iteration_input(),
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                let _ = control_tx.send(WorkerControl::Reflect);
                                app.status = "Running reflection suite".to_string();
                                app.message = "Running reflection suite from the paused TUI state."
                                    .to_string();
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                let _ = control_tx.send(WorkerControl::Exit);
                                app.should_quit = true;
                            }
                            _ => {}
                        }
                    } else if app.post_run_actions_available {
                        match key.code {
                            KeyCode::Char('n') => {
                                let _ = control_tx.send(WorkerControl::ExtendIterations(1));
                                app.post_run_actions_available = false;
                                app.reflect_available = false;
                                app.status = "Resuming with 1 more iteration".to_string();
                                app.push_activity(
                                    "User restarted the finished run with 1 additional iteration"
                                        .to_string(),
                                );
                                app.set_default_message();
                                worker_stopped = false;
                            }
                            KeyCode::Char('x') | KeyCode::Char('X') => app.open_iteration_input(),
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                let _ = control_tx.send(WorkerControl::Reflect);
                                app.status = "Running reflection suite".to_string();
                                app.message =
                                    "Running reflection suite from the finished TUI state."
                                        .to_string();
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                let _ = control_tx.send(WorkerControl::Exit);
                                app.should_quit = true;
                            }
                            _ => {}
                        }
                    } else if app.reflect_available {
                        match key.code {
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                let _ = control_tx.send(WorkerControl::Reflect);
                                app.status = "Running reflection suite".to_string();
                                app.message =
                                    "Running reflection suite from the finished TUI state."
                                        .to_string();
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                let _ = control_tx.send(WorkerControl::Exit);
                                app.should_quit = true;
                            }
                            _ => {}
                        }
                    } else if matches!(key.code, KeyCode::Char('n')) {
                        let _ = control_tx.send(WorkerControl::ExtendIterations(1));
                        app.status = "Queued 1 more iteration".to_string();
                        app.message = "Added 1 more iteration to the current run.".to_string();
                        app.push_activity(
                            "Queued 1 additional iteration for the active run".to_string(),
                        );
                    } else if matches!(key.code, KeyCode::Char('x') | KeyCode::Char('X')) {
                        app.open_iteration_input();
                    } else if is_graceful_stop_key(key) && !app.graceful_quit_requested {
                        graceful_quit.store(true, Ordering::Relaxed);
                        app.graceful_quit_requested = true;
                        app.set_default_message();
                        app.push_activity("Graceful stop requested by user".to_string());
                    } else if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                        app.should_quit = true;
                    }
                }
                CEvent::Mouse(mouse) => {
                    let area = terminal.size()?;
                    handle_run_mouse_event(&mut app, mouse, area.into());
                }
                CEvent::Resize(_, _) => {
                    let area: Rect = terminal.size()?.into();
                    let layout = run_layout(area, &app);
                    if let Some(terminal_area) = layout.terminal {
                        if let Err(error) = app.sync_terminal_size(terminal_area) {
                            app.push_activity(format!(
                                "Embedded terminal resize failed: {error:#}"
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        if last_redraw.elapsed() >= tick_rate {
            if app.spinner_label.is_some() || app.has_running_calls() {
                app.spinner_frame = (app.spinner_frame + 1) % 4;
            }
            if app.terminal_visible() {
                let area: Rect = terminal.size()?.into();
                let layout = run_layout(area, &app);
                if let Some(terminal_area) = layout.terminal {
                    if let Err(error) = app.sync_terminal_size(terminal_area) {
                        app.push_activity(format!("Embedded terminal resize failed: {error:#}"));
                    }
                }
            }
            terminal.draw(|frame| draw_run_ui(frame, &app))?;
            last_redraw = Instant::now();
        }

        if app.should_quit {
            break;
        }

        if worker_stopped && app.spinner_label.is_some() {
            app.spinner_label = None;
        }
    }

    Ok(())
}

fn run_layout(area: Rect, app: &UiApp) -> RunLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(15),
            Constraint::Min(10),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    let upper = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(vertical[0]);
    let upper_left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(1)])
        .split(upper[0]);

    let visible_columns = app.visible_bottom_columns();
    let bottom_chunks = if visible_columns.is_empty() {
        Vec::new()
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints(
                visible_columns
                    .iter()
                    .map(|_| Constraint::Ratio(1, visible_columns.len() as u32))
                    .collect::<Vec<_>>(),
            )
            .split(vertical[1])
            .to_vec()
    };

    let mut layout = RunLayout {
        header: upper_left[0],
        progress: upper_left[1],
        issue_details: upper[1],
        output: None,
        diff: None,
        side: None,
        terminal: None,
        messages: vertical[2],
        footer: vertical[3],
    };

    for (column, rect) in visible_columns.iter().zip(bottom_chunks.into_iter()) {
        match column {
            BottomColumn::Output => layout.output = Some(rect),
            BottomColumn::Diff => layout.diff = Some(rect),
            BottomColumn::Terminal => layout.terminal = Some(rect),
            BottomColumn::Side => {
                let side = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(50),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                    ])
                    .split(rect);
                layout.side = Some(SideLayout {
                    activity: side[0],
                    timeline: side[1],
                    subagent: side[2],
                });
            }
        }
    }

    layout
}

fn centered_rect(area: Rect, width_percent: u16, height: u16) -> Rect {
    let popup_width = area
        .width
        .saturating_mul(width_percent)
        .checked_div(100)
        .unwrap_or(area.width)
        .max(24)
        .min(area.width);
    let popup_height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

fn visible_line_capacity(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

fn content_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

fn wrapped_row_count_for_line(line: &str, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let chars = line.chars().count();
    chars.div_ceil(width).max(1)
}

fn wrapped_row_count_for_lines(lines: &VecDeque<String>, area: Rect) -> usize {
    let width = content_width(area);
    if lines.is_empty() {
        return 1;
    }
    lines
        .iter()
        .map(|line| wrapped_row_count_for_line(line, width))
        .sum()
}

fn wrapped_row_count_for_slice(lines: &[String], area: Rect) -> usize {
    let width = content_width(area);
    if lines.is_empty() {
        return 1;
    }
    lines
        .iter()
        .map(|line| wrapped_row_count_for_line(line, width))
        .sum()
}

fn wrapped_row_count_for_text(text: &str, area: Rect) -> usize {
    let width = content_width(area);
    split_for_ui(text)
        .into_iter()
        .map(|line| wrapped_row_count_for_line(&line, width))
        .sum()
}

fn max_scroll(lines_len: usize, area: Rect) -> usize {
    lines_len.saturating_sub(visible_line_capacity(area))
}

fn resolve_scroll(scroll: u16, lines_len: usize, area: Rect) -> u16 {
    let max = max_scroll(lines_len, area);
    if scroll == AUTO_SCROLL {
        max.min(u16::MAX as usize) as u16
    } else {
        (scroll as usize).min(max).min(u16::MAX as usize) as u16
    }
}

fn apply_scroll_delta(scroll: &mut u16, lines_len: usize, area: Rect, mouse_kind: MouseEventKind) {
    let max = max_scroll(lines_len, area);
    if max == 0 {
        *scroll = 0;
        return;
    }

    let mut current = if *scroll == AUTO_SCROLL {
        max
    } else {
        (*scroll as usize).min(max)
    };

    match mouse_kind {
        MouseEventKind::ScrollUp => {
            current = current.saturating_sub(SCROLL_STEP);
        }
        MouseEventKind::ScrollDown => {
            current = (current + SCROLL_STEP).min(max);
        }
        _ => return,
    }

    if current >= max {
        *scroll = AUTO_SCROLL;
    } else {
        *scroll = current.min(u16::MAX as usize) as u16;
    }
}

fn point_in_rect(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn run_scroll_target(layout: RunLayout, column: u16, row: u16) -> Option<ScrollTarget> {
    if point_in_rect(layout.progress, column, row) {
        Some(ScrollTarget::Progress)
    } else if point_in_rect(layout.issue_details, column, row) {
        Some(ScrollTarget::IssueDetails)
    } else if layout
        .side
        .map(|side| point_in_rect(side.activity, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Activity)
    } else if layout
        .output
        .map(|rect| point_in_rect(rect, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Output)
    } else if layout
        .diff
        .map(|rect| point_in_rect(rect, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Diff)
    } else if layout
        .terminal
        .map(|rect| point_in_rect(rect, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Terminal)
    } else if layout
        .side
        .map(|side| point_in_rect(side.timeline, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Timeline)
    } else if layout
        .side
        .map(|side| point_in_rect(side.subagent, column, row))
        .unwrap_or(false)
    {
        Some(ScrollTarget::Subagent)
    } else {
        None
    }
}

fn handle_run_mouse_event(app: &mut UiApp, mouse: MouseEvent, area: Rect) {
    let layout = run_layout(area, app);
    let terminal_hit = layout
        .terminal
        .map(|rect| point_in_rect(rect, mouse.column, mouse.row))
        .unwrap_or(false);

    match mouse.kind {
        MouseEventKind::Down(_) => {
            if terminal_hit {
                app.focus_terminal();
            } else {
                app.focus_ralph();
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if terminal_hit {
                app.focus_terminal();
                app.scroll_terminal(mouse.kind);
                return;
            }
            app.focus_ralph();
        }
        _ => return,
    }

    let spinner = SPINNER_FRAMES[app.spinner_frame];
    let tool_lines = app.tool_panel_lines(spinner);
    let subagent_lines = app.subagent_panel_lines(spinner);
    let target = run_scroll_target(layout, mouse.column, mouse.row);
    match target {
        Some(ScrollTarget::Progress) => apply_scroll_delta(
            &mut app.progress_scroll,
            wrapped_row_count_for_lines(&app.progress_lines, layout.progress),
            layout.progress,
            mouse.kind,
        ),
        Some(ScrollTarget::IssueDetails) => apply_scroll_delta(
            &mut app.issue_details_scroll,
            wrapped_row_count_for_text(&app.issue_details, layout.issue_details),
            layout.issue_details,
            mouse.kind,
        ),
        Some(ScrollTarget::Activity) => apply_scroll_delta(
            &mut app.activity_scroll,
            wrapped_row_count_for_lines(
                &app.activity_lines,
                layout.side.expect("side layout missing").activity,
            ),
            layout.side.expect("side layout missing").activity,
            mouse.kind,
        ),
        Some(ScrollTarget::Output) => apply_scroll_delta(
            &mut app.output_scroll,
            wrapped_row_count_for_lines(&app.output_lines, layout.output.expect("output missing")),
            layout.output.expect("output missing"),
            mouse.kind,
        ),
        Some(ScrollTarget::Diff) => apply_scroll_delta(
            &mut app.diff_scroll,
            wrapped_row_count_for_lines(&app.diff_lines, layout.diff.expect("diff missing")),
            layout.diff.expect("diff missing"),
            mouse.kind,
        ),
        Some(ScrollTarget::Terminal) => {}
        Some(ScrollTarget::Timeline) => apply_scroll_delta(
            &mut app.timeline_scroll,
            wrapped_row_count_for_slice(
                &tool_lines,
                layout.side.expect("side layout missing").timeline,
            ),
            layout.side.expect("side layout missing").timeline,
            mouse.kind,
        ),
        Some(ScrollTarget::Subagent) => apply_scroll_delta(
            &mut app.subagent_scroll,
            wrapped_row_count_for_slice(
                &subagent_lines,
                layout.side.expect("side layout missing").subagent,
            ),
            layout.side.expect("side layout missing").subagent,
            mouse.kind,
        ),
        None => {}
    }
}

fn draw_run_ui(frame: &mut Frame, app: &UiApp) {
    let layout = run_layout(frame.area(), app);
    let title = "Ralph";

    let spinner_frames = SPINNER_FRAMES;
    let spinner_line = if let Some(label) = &app.spinner_label {
        format!("Claude: {} {}", spinner_frames[app.spinner_frame], label)
    } else {
        "Claude: idle".to_string()
    };
    let usage_line = format_usage_inline(&app.usage, app.total_cost_usd);

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Status: ",
                Style::default()
                    .fg(ACCENT_INFO)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.status.clone(), Style::default().fg(FG_MAIN)),
        ]),
        Line::from(vec![
            Span::styled(
                "Issue:  ",
                Style::default()
                    .fg(ACCENT_INFO)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.issue.clone(), Style::default().fg(FG_MAIN)),
        ]),
        Line::from(Span::styled(
            app.summary.clone(),
            Style::default().fg(ACCENT_PROGRESS),
        )),
        Line::from(Span::styled(usage_line, Style::default().fg(ACCENT_INFO))),
        Line::from(Span::styled(spinner_line, Style::default().fg(ACCENT_WARN))),
    ])
    .style(Style::default().fg(FG_MAIN).bg(BG_HEADER))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT_INFO))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(ACCENT_INFO)
                    .add_modifier(Modifier::BOLD),
            )),
    );

    let progress = Paragraph::new(lines_from(&app.progress_lines))
        .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_PROGRESS))
                .title(Span::styled(
                    "Tool Log",
                    Style::default()
                        .fg(ACCENT_PROGRESS)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .scroll((
            resolve_scroll(
                app.progress_scroll,
                wrapped_row_count_for_lines(&app.progress_lines, layout.progress),
                layout.progress,
            ),
            0,
        ))
        .wrap(Wrap { trim: false });

    let issue_details = Paragraph::new(app.issue_details.clone())
        .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_WARN))
                .title(Span::styled(
                    "Issue Details (bd show)",
                    Style::default()
                        .fg(ACCENT_WARN)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .scroll((
            resolve_scroll(
                app.issue_details_scroll,
                wrapped_row_count_for_text(&app.issue_details, layout.issue_details),
                layout.issue_details,
            ),
            0,
        ))
        .wrap(Wrap { trim: false });

    let activity = layout.side.map(|side| {
        let activity_lines = if app.activity_lines.is_empty() {
            vec![Line::from("No Claude activity yet.")]
        } else {
            lines_from(&app.activity_lines)
        };
        Paragraph::new(activity_lines)
            .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_ACTIVITY))
                    .title(Span::styled(
                        "Claude Activity (Verbose)",
                        Style::default()
                            .fg(ACCENT_ACTIVITY)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((
                resolve_scroll(
                    app.activity_scroll,
                    wrapped_row_count_for_lines(&app.activity_lines, side.activity),
                    side.activity,
                ),
                0,
            ))
            .wrap(Wrap { trim: false })
    });

    let output = layout.output.map(|output_rect| {
        Paragraph::new(lines_from_output(&app.output_lines, output_rect))
            .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_OUTPUT))
                    .title(Span::styled(
                        "Claude Output (Narrative)",
                        Style::default()
                            .fg(ACCENT_OUTPUT)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((
                resolve_scroll(
                    app.output_scroll,
                    wrapped_row_count_for_lines(&app.output_lines, output_rect),
                    output_rect,
                ),
                0,
            ))
            .wrap(Wrap { trim: false })
    });

    let terminal_border = if app.terminal_focused() {
        ACCENT_INFO
    } else {
        ACCENT_DIFF_HUNK
    };
    let terminal_title = if app.terminal_focused() {
        app.embedded_terminal
            .as_ref()
            .map(|terminal| format!("Terminal [{}] [FOCUSED]", terminal.shell_label()))
            .unwrap_or_else(|| "Terminal [FOCUSED]".to_string())
    } else {
        app.embedded_terminal
            .as_ref()
            .map(|terminal| format!("Terminal [{}]", terminal.shell_label()))
            .unwrap_or_else(|| "Terminal".to_string())
    };
    let terminal_snapshot = app
        .embedded_terminal
        .as_ref()
        .map(|terminal| terminal.snapshot());
    let terminal_pane = layout.terminal.map(|_| {
        let terminal_lines = terminal_snapshot
            .as_ref()
            .map(|snapshot| snapshot.lines.clone())
            .unwrap_or_else(|| {
                vec![Line::from(app.terminal_error.clone().unwrap_or_else(
                    || "Embedded terminal unavailable.".to_string(),
                ))]
            });
        Paragraph::new(terminal_lines)
            .style(Style::default())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(terminal_border))
                    .title(Span::styled(
                        terminal_title,
                        Style::default()
                            .fg(terminal_border)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .wrap(Wrap { trim: false })
    });

    let diff_pane = layout.diff.map(|diff_rect| {
        Paragraph::new(lines_from_diff(&app.diff_lines, diff_rect))
            .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_DIFF_HUNK))
                    .title(Span::styled(
                        "Code Diffs",
                        Style::default()
                            .fg(ACCENT_DIFF_HUNK)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((
                resolve_scroll(
                    app.diff_scroll,
                    wrapped_row_count_for_lines(&app.diff_lines, diff_rect),
                    diff_rect,
                ),
                0,
            ))
            .wrap(Wrap { trim: false })
    });

    let tool_panel_lines = app.tool_panel_lines(spinner_frames[app.spinner_frame]);
    let subagent_panel_lines = app.subagent_panel_lines(spinner_frames[app.spinner_frame]);

    let timeline = layout.side.map(|side| {
        Paragraph::new(lines_from_slice(&tool_panel_lines))
            .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_INFO))
                    .title(Span::styled(
                        "Tools",
                        Style::default()
                            .fg(ACCENT_INFO)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((
                resolve_scroll(
                    app.timeline_scroll,
                    wrapped_row_count_for_slice(&tool_panel_lines, side.timeline),
                    side.timeline,
                ),
                0,
            ))
            .wrap(Wrap { trim: false })
    });

    let subagent = layout.side.map(|side| {
        Paragraph::new(lines_from_slice(&subagent_panel_lines))
            .style(Style::default().fg(FG_MAIN).bg(BG_PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_ACTIVITY))
                    .title(Span::styled(
                        "Subagents",
                        Style::default()
                            .fg(ACCENT_ACTIVITY)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((
                resolve_scroll(
                    app.subagent_scroll,
                    wrapped_row_count_for_slice(&subagent_panel_lines, side.subagent),
                    side.subagent,
                ),
                0,
            ))
            .wrap(Wrap { trim: false })
    });

    let messages = Paragraph::new(app.message_text())
        .style(Style::default().fg(FG_MAIN).bg(BG_FOOTER))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_INFO))
                .title(Span::styled(
                    "Messages",
                    Style::default()
                        .fg(ACCENT_INFO)
                        .add_modifier(Modifier::BOLD),
                )),
        );

    let footer = Paragraph::new(app.controls_text())
        .style(Style::default().fg(FG_MUTED).bg(BG_FOOTER))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(FG_MUTED))
                .title(Span::styled(
                    "Controls",
                    Style::default().fg(FG_MAIN).add_modifier(Modifier::BOLD),
                )),
        );

    frame.render_widget(
        Block::default().style(Style::default().bg(BG_MAIN)),
        frame.area(),
    );
    frame.render_widget(header, layout.header);
    frame.render_widget(progress, layout.progress);
    frame.render_widget(issue_details, layout.issue_details);
    if let (Some(widget), Some(side)) = (activity, layout.side) {
        frame.render_widget(widget, side.activity);
    }
    if let (Some(widget), Some(output_rect)) = (output, layout.output) {
        frame.render_widget(widget, output_rect);
    }
    if let (Some(widget), Some(diff_rect)) = (diff_pane, layout.diff) {
        frame.render_widget(widget, diff_rect);
    }
    if let (Some(widget), Some(terminal_rect)) = (terminal_pane, layout.terminal) {
        frame.render_widget(widget, terminal_rect);
    }
    if let (Some(widget), Some(side)) = (timeline, layout.side) {
        frame.render_widget(widget, side.timeline);
    }
    if let (Some(widget), Some(side)) = (subagent, layout.side) {
        frame.render_widget(widget, side.subagent);
    }
    frame.render_widget(messages, layout.messages);
    frame.render_widget(footer, layout.footer);

    if app.terminal_visible() && app.terminal_focused() {
        if let Some(snapshot) = terminal_snapshot.as_ref() {
            if !snapshot.hide_cursor {
                if let Some((cursor_row, cursor_col)) = snapshot.cursor {
                    if let Some(terminal_rect) = layout.terminal {
                        let x = terminal_rect.x.saturating_add(1).saturating_add(cursor_col);
                        let y = terminal_rect.y.saturating_add(1).saturating_add(cursor_row);
                        if x < terminal_rect.right().saturating_sub(1)
                            && y < terminal_rect.bottom().saturating_sub(1)
                        {
                            frame.set_cursor_position((x, y));
                        }
                    }
                }
            }
        }
    }

    if app.iteration_input_mode {
        let popup_area = centered_rect(frame.area(), 42, 7);
        let help_line = app
            .iteration_input_error
            .clone()
            .unwrap_or_else(|| "Type a positive number, then press Enter to resume.".to_string());
        let popup = Paragraph::new(vec![
            Line::from("How many more iterations should Ralph run?"),
            Line::from(String::new()),
            Line::from(vec![
                Span::styled(
                    "More iterations: ",
                    Style::default()
                        .fg(ACCENT_INFO)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    app.iteration_input_value.clone(),
                    Style::default().fg(FG_MAIN),
                ),
            ]),
            Line::from(Span::styled(help_line, Style::default().fg(ACCENT_WARN))),
        ])
        .style(Style::default().fg(FG_MAIN).bg(BG_HEADER))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_INFO))
                .title(Span::styled(
                    "Add Iterations",
                    Style::default()
                        .fg(ACCENT_INFO)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .wrap(Wrap { trim: false });

        frame.render_widget(Clear, popup_area);
        frame.render_widget(popup, popup_area);
    }
}

fn lines_from(lines: &VecDeque<String>) -> Vec<Line<'static>> {
    if lines.is_empty() {
        vec![Line::from(String::new())]
    } else {
        lines.iter().cloned().map(Line::from).collect()
    }
}

fn lines_from_slice(lines: &[String]) -> Vec<Line<'static>> {
    if lines.is_empty() {
        vec![Line::from(String::new())]
    } else {
        lines.iter().cloned().map(Line::from).collect()
    }
}

fn lines_from_output(lines: &VecDeque<String>, area: Rect) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::from(String::new())];
    }

    let width = content_width(area);
    let mut rendered = Vec::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("Δ ") {
            rendered.extend(styled_rows(rest, diff_line_style(rest), width));
        } else if line == "Δ" {
            rendered.extend(styled_rows("", diff_line_style(""), width));
        } else {
            rendered.push(Line::from(Span::styled(
                line.clone(),
                Style::default().fg(FG_MAIN),
            )));
        }
    }
    rendered
}

fn lines_from_diff(lines: &VecDeque<String>, area: Rect) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::from(String::new())];
    }

    let width = content_width(area);
    let mut rendered = Vec::new();
    for line in lines {
        rendered.extend(styled_rows(line, diff_line_style(line), width));
    }
    rendered
}

fn styled_rows(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(Span::styled(text.to_string(), style))];
    }

    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![Line::from(Span::styled(" ".repeat(width), style))];
    }

    chars
        .chunks(width)
        .map(|chunk| {
            let segment = chunk.iter().collect::<String>();
            Line::from(Span::styled(pad_to_width(&segment, width), style))
        })
        .collect()
}

fn pad_to_width(text: &str, width: usize) -> String {
    let char_count = text.chars().count();
    if char_count >= width {
        return text.to_string();
    }

    let mut padded = String::with_capacity(text.len() + (width - char_count));
    padded.push_str(text);
    padded.push_str(&" ".repeat(width - char_count));
    padded
}

fn diff_line_style(line: &str) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::default()
            .fg(ACCENT_DIFF_HUNK)
            .bg(BG_DIFF_HUNK)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(ACCENT_DIFF_ADD).bg(BG_DIFF_ADD)
    } else if line.starts_with('-') {
        Style::default().fg(ACCENT_DIFF_REMOVE).bg(BG_DIFF_REMOVE)
    } else if line.starts_with("@@") || line.starts_with("diff ") {
        Style::default()
            .fg(ACCENT_DIFF_HUNK)
            .bg(BG_DIFF_HUNK)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(FG_MAIN)
    }
}

fn format_usage_inline(usage: &UsageTally, total_cost_usd: f64) -> String {
    format!(
        "Usage | in={} out={} cache_read={} cache_write={} cost=${:.4}",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        total_cost_usd
    )
}

fn status_label(status: LiveCallStatus) -> &'static str {
    match status {
        LiveCallStatus::Running => "running",
        LiveCallStatus::Ok => "ok",
        LiveCallStatus::Error => "error",
    }
}

fn status_marker(status: LiveCallStatus) -> &'static str {
    match status {
        LiveCallStatus::Running => ">",
        LiveCallStatus::Ok => "done",
        LiveCallStatus::Error => "fail",
    }
}

fn runtime_label(status: LiveCallStatus, started_at: Instant, duration_ms: Option<u128>) -> String {
    let ms = match status {
        LiveCallStatus::Running => Instant::now().duration_since(started_at).as_millis(),
        LiveCallStatus::Ok | LiveCallStatus::Error => {
            duration_ms.unwrap_or_else(|| Instant::now().duration_since(started_at).as_millis())
        }
    };
    format_duration_ms(ms)
}

fn format_duration_ms(ms: u128) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    }
}

fn init_terminal() -> Result<DefaultTerminal> {
    enable_raw_mode().context("failed to enable raw mode")?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("failed to enter alternate screen")?;
    io::stdout()
        .execute(EnableMouseCapture)
        .context("failed to enable mouse capture")?;
    Ok(ratatui::init())
}

fn restore_terminal(terminal: &mut DefaultTerminal) -> Result<()> {
    ratatui::restore();
    disable_raw_mode().ok();
    terminal.backend_mut().execute(DisableMouseCapture).ok();
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    Ok(())
}

fn send(ui_tx: &Sender<UiEvent>, event: UiEvent) {
    let _ = ui_tx.send(event);
}

fn send_activity(
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Some(logs) = debug_logs.as_mut() {
        logs.log_activity(&message);
    }
    send(ui_tx, UiEvent::Activity(message));
}

fn send_output_line(
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    line: impl Into<String>,
) {
    let line = line.into();
    if let Some(logs) = debug_logs.as_mut() {
        let mut with_newline = line.clone();
        with_newline.push('\n');
        logs.log_output_chunk(&with_newline);
    }
    send(ui_tx, UiEvent::Output(line));
}

fn emit_iteration_output_boundary(
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    iteration: usize,
    total_iterations: usize,
    issue_id: &str,
) {
    let divider = "=".repeat(72);
    send_output_line(ui_tx, debug_logs, String::new());
    send_output_line(ui_tx, debug_logs, divider.clone());
    send_output_line(
        ui_tx,
        debug_logs,
        format!("Claude Output | Iteration {iteration}/{total_iterations} | Issue {issue_id}"),
    );
    send_output_line(ui_tx, debug_logs, divider);
    send_output_line(ui_tx, debug_logs, String::new());
}

fn emit_named_output_boundary(
    ui_tx: &Sender<UiEvent>,
    debug_logs: &mut Option<DebugLogs>,
    label: impl AsRef<str>,
) {
    let divider = "=".repeat(72);
    send_output_line(ui_tx, debug_logs, String::new());
    send_output_line(ui_tx, debug_logs, divider.clone());
    send_output_line(
        ui_tx,
        debug_logs,
        format!("Claude Output | {}", label.as_ref()),
    );
    send_output_line(ui_tx, debug_logs, divider);
    send_output_line(ui_tx, debug_logs, String::new());
}

fn send_output_chunk(ui_tx: &Sender<UiEvent>, debug_logs: &mut Option<DebugLogs>, chunk: String) {
    if let Some(logs) = debug_logs.as_mut() {
        logs.log_output_chunk(&chunk);
    }
    send(ui_tx, UiEvent::OutputChunk(chunk));
}

fn write_issue_snapshot(paths: &Paths, run_id: Option<&str>) -> Result<()> {
    issues::write_issue_snapshot(paths, run_id)
}

fn ensure_issue_snapshot_consistency(paths: &Paths) -> Result<()> {
    issues::ensure_issue_snapshot_consistency(paths)
}

fn get_next_issue() -> Result<Option<String>> {
    issues::get_next_issue()
}

fn get_issue_details(issue_id: &str) -> Result<String> {
    issues::get_issue_details(issue_id)
}

fn build_prompt(paths: &Paths, issue_id: &str, issue_details: &str) -> String {
    prompts::build_issue_prompt(
        paths,
        DEFAULT_META_PROMPT,
        DEFAULT_ISSUE_PROMPT,
        issue_id,
        issue_details,
    )
}

fn build_cleanup_prompt(
    paths: &Paths,
    issue_id: Option<&str>,
    issue_details: &str,
    trigger: &str,
) -> String {
    prompts::build_cleanup_prompt(
        paths,
        DEFAULT_META_PROMPT,
        DEFAULT_CLEANUP_PROMPT,
        issue_id,
        issue_details,
        trigger,
    )
}

fn build_repair_prompt(paths: &Paths, trigger: &str, remaining_count: usize) -> String {
    prompts::build_repair_prompt(
        paths,
        DEFAULT_META_PROMPT,
        DEFAULT_REPAIR_PROMPT,
        trigger,
        remaining_count,
    )
}

fn build_reflection_prompt(
    paths: &Paths,
    prompt_path: &Path,
    fallback_prompt: &str,
    pass_name: &str,
    trigger: &str,
) -> String {
    prompts::build_reflection_prompt(
        paths,
        DEFAULT_META_PROMPT,
        prompt_path,
        fallback_prompt,
        pass_name,
        trigger,
    )
}

fn acquire_run_lock(paths: &Paths) -> Result<run_state::RunLockGuard> {
    run_state::acquire_run_lock(paths)
}

fn write_run_state(paths: &Paths, state: &RunState) -> Result<()> {
    run_state::write_run_state(paths, state)
}

fn read_run_state(paths: &Paths) -> Option<RunState> {
    run_state::read_run_state(paths)
}

fn update_run_state_progress(
    paths: &Paths,
    run_id: &str,
    current_issue: Option<String>,
    iteration: usize,
    total_iterations: usize,
    status: &str,
) -> Result<()> {
    run_state::update_run_state_progress(
        paths,
        run_id,
        current_issue,
        iteration,
        total_iterations,
        status,
    )
}

fn mark_run_state_finished(paths: &Paths, run_id: &str, status: &str) -> Result<()> {
    run_state::mark_run_state_finished(paths, run_id, status)
}

fn run_claude(
    cli: &Cli,
    ui_tx: &Sender<UiEvent>,
    issue_id: &str,
    prompt: &str,
    debug_logs: &mut Option<DebugLogs>,
) -> Result<ClaudeOutcome> {
    claude::run_claude(cli, ui_tx, issue_id, prompt, debug_logs)
}

fn run_capture<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let settings = runtime_settings();
    capture::run_capture(args, settings.capture_timeout, settings.capture_retries)
}
