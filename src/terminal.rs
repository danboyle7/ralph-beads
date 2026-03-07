use std::env;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, ExitStatus, MasterPty, PtySize};
use ratatui::layout::Rect;
use ratatui::style::{Color as TuiColor, Modifier, Style};
use ratatui::text::{Line, Span};

const DEFAULT_SHELL: &str = "/bin/zsh";
const DEFAULT_TERM: &str = "xterm-256color";

pub(crate) struct TerminalSnapshot {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) cursor: Option<(u16, u16)>,
    pub(crate) hide_cursor: bool,
}

pub(crate) struct EmbeddedTerminal {
    shell_label: String,
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    _reader_thread: thread::JoinHandle<()>,
}

impl EmbeddedTerminal {
    pub(crate) fn spawn(cwd: &Path, area: Rect, scrollback_len: usize) -> Result<Self> {
        let size = pty_size_from_area(area);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size)
            .context("failed to open terminal PTY")?;

        let shell = env::var("SHELL").unwrap_or_else(|_| DEFAULT_SHELL.to_string());
        let shell_label = Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(shell.as_str())
            .to_string();

        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        command.arg("-i");
        command.env("TERM", DEFAULT_TERM);

        let child = pair
            .slave
            .spawn_command(command)
            .context("failed to spawn interactive shell")?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone terminal reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to take terminal writer")?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            scrollback_len,
        )));
        let parser_for_thread = Arc::clone(&parser);
        let reader_thread = thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Ok(mut parser) = parser_for_thread.lock() else {
                            break;
                        };
                        parser.process(&buffer[..read]);
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            shell_label,
            parser,
            master: pair.master,
            writer,
            child,
            _reader_thread: reader_thread,
        })
    }

    pub(crate) fn shell_label(&self) -> &str {
        &self.shell_label
    }

    pub(crate) fn resize(&mut self, area: Rect) -> Result<()> {
        let size = pty_size_from_area(area);
        self.master
            .resize(size)
            .context("failed to resize terminal PTY")?;
        let mut parser = self
            .parser
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal parser lock poisoned"))?;
        parser.screen_mut().set_size(size.rows, size.cols);
        Ok(())
    }

    pub(crate) fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_scrollback(0);
        }
        self.writer
            .write_all(bytes)
            .context("failed to write terminal input")?;
        self.writer.flush().ok();
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> TerminalSnapshot {
        let Ok(parser) = self.parser.lock() else {
            return TerminalSnapshot {
                lines: vec![Line::from("Terminal unavailable.")],
                cursor: None,
                hide_cursor: true,
            };
        };
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let scrollback = screen.scrollback();
        let mut lines = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut current_style = None;
            let mut current_text = String::new();
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    flush_span(&mut spans, &mut current_text, current_style.take());
                    current_text.push(' ');
                    current_style = Some(Style::default());
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let style = style_from_cell(cell);
                let text = if cell.has_contents() {
                    cell.contents().to_string()
                } else {
                    " ".to_string()
                };
                if current_style == Some(style) {
                    current_text.push_str(&text);
                } else {
                    flush_span(&mut spans, &mut current_text, current_style.take());
                    current_text = text;
                    current_style = Some(style);
                }
            }
            flush_span(&mut spans, &mut current_text, current_style.take());
            if spans.is_empty() {
                spans.push(Span::raw(String::new()));
            }
            lines.push(Line::from(spans));
        }
        let cursor = Some(screen.cursor_position());
        TerminalSnapshot {
            lines,
            cursor,
            hide_cursor: screen.hide_cursor() || scrollback > 0,
        }
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("failed to poll embedded terminal process")
    }

    pub(crate) fn scroll_scrollback(&mut self, delta: isize) {
        let Ok(mut parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen_mut();
        let current = screen.scrollback();
        let next = if delta.is_negative() {
            current.saturating_add(delta.unsigned_abs())
        } else {
            current.saturating_sub(delta as usize)
        };
        screen.set_scrollback(next);
    }
}

impl Drop for EmbeddedTerminal {
    fn drop(&mut self) {
        let _ = self.writer.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn terminal_input_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Char(ch) => encode_char(ch, key.modifiers),
        KeyCode::F(number) => encode_function_key(number),
        _ => None,
    }
}

fn encode_char(ch: char, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        let lower = ch.to_ascii_lowercase();
        let code = match lower {
            '@' | ' ' => 0x00,
            'a'..='z' => (lower as u8) - b'a' + 0x01,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' => 0x1e,
            '_' => 0x1f,
            _ => return None,
        };
        return Some(vec![code]);
    }

    let mut bytes = Vec::new();
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    let mut encoded = [0_u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
    Some(bytes)
}

fn encode_function_key(number: u8) -> Option<Vec<u8>> {
    let sequence = match number {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        9 => "\x1b[20~",
        10 => "\x1b[21~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    Some(sequence.as_bytes().to_vec())
}

fn pty_size_from_area(area: Rect) -> PtySize {
    PtySize {
        rows: area.height.saturating_sub(2).max(1),
        cols: area.width.saturating_sub(2).max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn flush_span(
    spans: &mut Vec<Span<'static>>,
    current_text: &mut String,
    current_style: Option<Style>,
) {
    if current_text.is_empty() {
        return;
    }
    let text = std::mem::take(current_text);
    match current_style {
        Some(style) => spans.push(Span::styled(text, style)),
        None => spans.push(Span::raw(text)),
    }
}

fn style_from_cell(cell: &vt100::Cell) -> Style {
    let mut fg = color_from_vt100(cell.fgcolor());
    let mut bg = color_from_vt100(cell.bgcolor());
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default().fg(fg).bg(bg);
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn color_from_vt100(color: vt100::Color) -> TuiColor {
    match color {
        vt100::Color::Default => TuiColor::Reset,
        vt100::Color::Idx(index) => TuiColor::Indexed(index),
        vt100::Color::Rgb(r, g, b) => TuiColor::Rgb(r, g, b),
    }
}
