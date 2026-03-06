use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde_json::{json, Value};

use crate::capture;
use crate::cli::Paths;

const SNAPSHOT_CAPTURE_TIMEOUT_SECONDS: u64 = 12;
const BD_QUERY_TIMEOUT_SECONDS: u64 = 12;

#[derive(Debug, Clone)]
struct IssueSnapshot {
    captured_at: String,
    captured_by_run_id: Option<String>,
    issue_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct IssueSnapshotReport {
    pub(crate) ok: bool,
    pub(crate) detail: String,
    pub(crate) baseline_count: usize,
    pub(crate) current_count: usize,
    pub(crate) missing_ids: Vec<String>,
}

pub(crate) fn get_open_issue_count() -> Result<usize> {
    let output = run_bd_query(
        [
            "bd",
            "list",
            "--status",
            "open",
            "--flat",
            "--json",
            "--no-pager",
        ],
        "open issue count",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    Ok(value.as_array().map(|items| items.len()).unwrap_or(0))
}

pub(crate) fn get_remaining_issue_count() -> Result<usize> {
    let output = run_bd_query(
        ["bd", "list", "--all", "--flat", "--json", "--no-pager"],
        "remaining issue count",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    Ok(value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    !item
                        .get("status")
                        .and_then(Value::as_str)
                        .map(|status| status.eq_ignore_ascii_case("closed"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0))
}

pub(crate) fn get_issue_status_map() -> Result<HashMap<String, String>> {
    let output = run_bd_query(
        ["bd", "list", "--all", "--flat", "--json", "--no-pager"],
        "issue status map",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    Ok(value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?;
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    Some((id.to_string(), status.to_string()))
                })
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default())
}

pub(crate) fn get_issue_type_map() -> Result<HashMap<String, String>> {
    let output = run_bd_query(
        ["bd", "list", "--all", "--flat", "--json", "--no-pager"],
        "issue type map",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    Ok(value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?;
                    let issue_type = issue_type_from_item(item)?;
                    Some((id.to_string(), issue_type))
                })
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default())
}

pub(crate) fn newly_closed_issue_ids(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<String> {
    let mut ids = after
        .iter()
        .filter_map(|(id, status_after)| {
            let was_closed = status_is_closed(before.get(id));
            let is_closed = status_after.eq_ignore_ascii_case("closed");
            if !was_closed && is_closed {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect::<Vec<String>>();
    ids.sort();
    ids
}

pub(crate) fn write_issue_snapshot(paths: &Paths, run_id: Option<&str>) -> Result<()> {
    let issue_ids = get_all_issue_ids()?;
    let snapshot = json!({
        "captured_at": Local::now().to_rfc3339(),
        "captured_by_run_id": run_id,
        "issue_count": issue_ids.len(),
        "issue_ids": issue_ids,
    });
    fs::write(
        &paths.issue_snapshot_file,
        serde_json::to_string_pretty(&snapshot)?,
    )
    .with_context(|| format!("failed to write {}", paths.issue_snapshot_file.display()))
}

pub(crate) fn issue_snapshot_consistency_report(paths: &Paths) -> Result<IssueSnapshotReport> {
    let current_ids = get_all_issue_ids()?;
    let current_count = current_ids.len();
    let Some(snapshot) = read_issue_snapshot(paths)? else {
        return Ok(IssueSnapshotReport {
            ok: true,
            detail: "no prior issue snapshot found yet; baseline will be created on next run"
                .to_string(),
            baseline_count: 0,
            current_count,
            missing_ids: Vec::new(),
        });
    };

    let current_set = current_ids.iter().collect::<HashSet<_>>();
    let missing_ids = snapshot
        .issue_ids
        .iter()
        .filter(|id| !current_set.contains(id))
        .cloned()
        .collect::<Vec<String>>();
    let baseline_count = snapshot.issue_ids.len();

    if missing_ids.is_empty() {
        return Ok(IssueSnapshotReport {
            ok: true,
            detail: format!(
                "snapshot baseline is consistent (baseline={} current={})",
                baseline_count, current_count
            ),
            baseline_count,
            current_count,
            missing_ids,
        });
    }

    let sample = missing_ids
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<String>>()
        .join(", ");
    let origin = snapshot
        .captured_by_run_id
        .as_deref()
        .map(|run| format!("run {run}"))
        .unwrap_or_else(|| "unknown run".to_string());

    Ok(IssueSnapshotReport {
        ok: false,
        detail: format!(
            "missing {} of {} issue IDs from last snapshot ({origin} at {}); sample: {}",
            missing_ids.len(),
            baseline_count,
            snapshot.captured_at,
            sample
        ),
        baseline_count,
        current_count,
        missing_ids,
    })
}

pub(crate) fn ensure_issue_snapshot_consistency(paths: &Paths) -> Result<()> {
    let report = issue_snapshot_consistency_report(paths)?;
    if report.ok {
        return Ok(());
    }
    bail!(
        "Issue snapshot mismatch detected: {}. This may indicate beads/dolt data loss or re-initialization. Inspect {} for prior snapshots before continuing.",
        report.detail,
        paths.archive_dir.display()
    )
}

pub(crate) fn get_next_issue() -> Result<Option<String>> {
    let args = ["bd", "ready", "--json"];
    let output = run_bd_query(args, "next issue query")?;
    let value: Value =
        serde_json::from_str(&output).with_context(|| format!("failed to parse {:?}", args))?;

    Ok(value.as_array().and_then(|items| next_ready_issue_id(items)))
}

pub(crate) fn get_issue_details(issue_id: &str) -> Result<String> {
    run_bd_query(["bd", "show", issue_id, "--no-pager"], "issue details")
        .or_else(|_| Ok(format!("Issue: {issue_id}")))
}

pub(crate) fn get_issue_type(issue_id: &str) -> Result<Option<String>> {
    let output = run_bd_query(
        ["bd", "show", issue_id, "--json", "--no-pager"],
        "issue type lookup",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd show JSON")?;
    Ok(issue_type_from_show_value(&value))
}

pub(crate) fn is_non_closed_issue(issue_id: &str) -> Result<bool> {
    let output = run_bd_query(
        ["bd", "list", "--all", "--flat", "--json", "--no-pager"],
        "interrupted issue check",
    )?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    Ok(value
        .as_array()
        .map(|items| {
            items.iter().any(|item| {
                let matches_issue = item
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| id == issue_id)
                    .unwrap_or(false);
                if !matches_issue {
                    return false;
                }
                !item
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| status.eq_ignore_ascii_case("closed"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false))
}

fn get_all_issue_ids() -> Result<Vec<String>> {
    let output = capture::run_capture(
        ["bd", "list", "--all", "--flat", "--json", "--no-pager"],
        Duration::from_secs(SNAPSHOT_CAPTURE_TIMEOUT_SECONDS),
        0,
    )
    .with_context(|| {
        format!(
            "snapshot consistency check could not load `bd list --all --flat --json` within {}s",
            SNAPSHOT_CAPTURE_TIMEOUT_SECONDS
        )
    })?;
    let value: Value = serde_json::from_str(&output).context("failed to parse bd list JSON")?;
    let mut ids = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn run_bd_query<I, S>(args: I, label: &str) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    capture::run_capture(args, Duration::from_secs(BD_QUERY_TIMEOUT_SECONDS), 0).with_context(
        || {
            format!(
                "{label} failed: `bd` command did not complete within {}s",
                BD_QUERY_TIMEOUT_SECONDS
            )
        },
    )
}

fn read_issue_snapshot(paths: &Paths) -> Result<Option<IssueSnapshot>> {
    if !paths.issue_snapshot_file.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&paths.issue_snapshot_file)
        .with_context(|| format!("failed to read {}", paths.issue_snapshot_file.display()))?;
    let value: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", paths.issue_snapshot_file.display()))?;
    let issue_ids = value
        .get("issue_ids")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    Ok(Some(IssueSnapshot {
        captured_at: value
            .get("captured_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        captured_by_run_id: value
            .get("captured_by_run_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        issue_ids,
    }))
}

fn status_is_closed(status: Option<&String>) -> bool {
    status
        .map(|value| value.eq_ignore_ascii_case("closed"))
        .unwrap_or(false)
}

fn issue_type_from_show_value(value: &Value) -> Option<String> {
    issue_type_from_item(value)
        .or_else(|| value.get("issue").and_then(issue_type_from_item))
        .or_else(|| {
            value
                .as_array()
                .and_then(|items| items.first())
                .and_then(issue_type_from_item)
        })
}

fn issue_type_from_item(item: &Value) -> Option<String> {
    [item.get("type"), item.get("issue_type"), item.get("kind")]
        .into_iter()
        .flatten()
        .find_map(json_value_to_label)
}

fn next_ready_issue_id(items: &[Value]) -> Option<String> {
    items.iter().find_map(|item| {
        if issue_type_from_item(item).as_deref() == Some("epic") {
            return None;
        }

        item.get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn json_value_to_label(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_ascii_lowercase());
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .map(|text| text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn next_ready_issue_skips_epics_and_keeps_ready_order() {
        let items = vec![
            json!({ "id": "BD-100", "type": "epic" }),
            json!({ "id": "BD-101", "type": "bug" }),
            json!({ "id": "BD-102", "type": "task" }),
        ];

        assert_eq!(next_ready_issue_id(&items), Some("BD-101".to_string()));
    }

    #[test]
    fn next_ready_issue_accepts_items_without_type_metadata() {
        let items = vec![json!({ "id": "BD-200" })];

        assert_eq!(next_ready_issue_id(&items), Some("BD-200".to_string()));
    }

    #[test]
    fn next_ready_issue_returns_none_when_only_epics_are_ready() {
        let items = vec![
            json!({ "id": "BD-300", "issue_type": "epic" }),
            json!({ "id": "BD-301", "kind": { "name": "epic" } }),
        ];

        assert_eq!(next_ready_issue_id(&items), None);
    }
}
