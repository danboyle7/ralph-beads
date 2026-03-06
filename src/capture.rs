use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

pub(crate) fn run_capture<I, S>(args: I, timeout: Duration, retries: usize) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    let command = iter
        .next()
        .map(|item| item.as_ref().to_string())
        .context("missing command")?;
    let args: Vec<String> = iter.map(|item| item.as_ref().to_string()).collect();

    let mut attempt = 0_usize;
    loop {
        match run_capture_once(&command, &args, timeout) {
            Ok(output) => return Ok(output),
            Err(error) => {
                let retryable = attempt < retries && is_transient_error_text(&error.to_string());
                if !retryable {
                    return Err(error);
                }
                attempt += 1;
                thread::sleep(Duration::from_millis(250 * attempt as u64));
            }
        }
    }
}

pub(crate) fn is_transient_error_text(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("timed out")
        || lowered.contains("temporary")
        || lowered.contains("temporarily unavailable")
        || lowered.contains("resource busy")
        || lowered.contains("connection reset")
        || lowered.contains("broken pipe")
}

fn run_capture_once(command: &str, args: &[String], timeout: Duration) -> Result<String> {
    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {command}"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture command stderr")?;

    // Drain pipes while the process runs; otherwise large outputs can deadlock.
    let stdout_reader = thread::spawn(move || {
        let mut reader = stdout;
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    });
    let stderr_reader = thread::spawn(move || {
        let mut reader = stderr;
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    });

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to poll {command} status"))?
        {
            break status;
        }
        if started.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            break child
                .wait()
                .with_context(|| format!("failed to wait on {command} after timeout"))?;
        }
        thread::sleep(Duration::from_millis(50));
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("stdout reader thread panicked"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("stderr reader thread panicked"))?;

    if timed_out {
        bail!("{command} timed out after {}s", timeout.as_secs());
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
        if stderr.is_empty() {
            bail!("{command} exited with {}", status);
        }
        bail!("{stderr}");
    }

    Ok(String::from_utf8_lossy(&stdout).to_string())
}
