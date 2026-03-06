# Ralph Beads

An automated workflow that uses Claude Code to iteratively process Beads issues.

## Overview

Ralph runs an issue loop:
1. Reads the next ready issue from Beads (`bd ready`)
2. Builds a composed prompt from:
   - `.ralph/prompts/ralph.md` (shared meta rules)
   - `.ralph/prompts/issue.md` (issue-mode instructions)
   - runtime issue/progress/rules context
3. Sends the prompt to `claude --dangerously-skip-permissions --print`
4. Claude implements and closes the issue
5. Repeats until done or `<promise>COMPLETE</promise>` is emitted

## Prerequisites

- [Claude Code CLI](https://docs.anthropic.com/claude-code) installed and authenticated
- `bd` (Beads) installed

## Installation

```bash
git clone https://github.com/youruser/ralph-beads.git
cargo install --path /path/to/ralph-beads --bin ralph --force
```

## Setup (per project)

```bash
cd your-project
ralph init
$EDITOR .ralph/prompts/issue.md
$EDITOR AGENTS.md
```

## Usage

```bash
# Standard loop (default 10 iterations)
ralph

# Custom iteration budget
ralph --iterations 20

# Process one issue
ralph --once

# Dry-run prompt output
ralph --dry-run

# Skip issue snapshot consistency checks (startup + preflight)
ralph --skip-snapshot-consistency

# Enable issue snapshot consistency checks (off by default)
ralph --snapshot-consistency

# Verbose activity output
ralph --verbose

# Recovery pass for interrupted work
ralph cleanup

# Run reflection once, then exit
ralph reflect

# Run reflection suite every 3 iterations
ralph --reflect-every 3

# Repair missing Ralph files/layout
ralph doctor

# Validate environment and project health before runs
ralph preflight

# Upgrade prompt templates safely (with backup)
ralph upgrade-prompts

# Print last run summary
ralph summary

# Print summary as JSON
ralph summary --json

# Show binary version with commit + dirty state
ralph --version
```

## Cleanup Behavior

- `cleanup` runs a dedicated cleanup prompt pass (`.ralph/prompts/cleanup.md`) and exits.
- If no interrupted issue is detected, `cleanup` exits as a no-op.
- Normal loop runs now auto-detect interrupted in-progress work from the previous run log and executes one cleanup pass before continuing.

## Reflection Behavior

- `reflect` runs three passes and exits:
  - `.ralph/prompts/quality-check.md`
  - `.ralph/prompts/code-review-check.md`
  - `.ralph/prompts/validation-check.md`
- `--reflect-every N` runs the same three passes every `N` loop iterations.

## Files

Repository defaults:

```text
ralph-beads/
├── ralph.md
├── issue.md
├── cleanup.md
├── quality-check.md
├── code-review-check.md
├── validation-check.md
└── src/
```

Project layout after `ralph init`:

```text
your-project/
├── AGENTS.md
└── .ralph/
    ├── prompts/
    │   ├── ralph.md
    │   ├── issue.md
    │   ├── cleanup.md
    │   ├── quality-check.md
    │   ├── code-review-check.md
    │   ├── validation-check.md
    │   └── .template-version
    ├── progress.txt
    ├── state.json
    ├── issue-snapshot.json
    ├── run.lock
    ├── config.toml
    ├── archive/
    ├── logs/
    └── .last-run
```

`ralph init` and `ralph doctor` will scaffold `AGENTS.md` if it does not exist.

## Progress Tracking

Ralph automatically:
- Appends progress to `.ralph/progress.txt`
- Archives previous run logs in `.ralph/archive/`
- Writes debug logs in `.ralph/logs/` when `--debug` is enabled
- Writes run state in `.ralph/state.json` and prevents concurrent runs with `.ralph/run.lock`
- Maintains a last-known issue snapshot in `.ralph/issue-snapshot.json` and checks for unexpected issue ID loss in preflight/startup

Add to project `.gitignore`:

```gitignore
.ralph/progress.txt
.ralph/archive/
.ralph/.last-run
.ralph/logs/
.ralph/state.json
.ralph/issue-snapshot.json
.ralph/run.lock
```

## Environment Variables

- `RALPH_MAX_ITERATIONS` overrides the default iteration budget.

## Versioning And Updates

- Ralph uses semantic versioning in `Cargo.toml` and includes build metadata in `--version`.
- `ralph --version` prints:
  - package version (for example `0.1.1`)
  - git commit short SHA
  - working tree state (`clean` or `dirty`)
- To ensure your installed binary is current after pulling changes:

```bash
cargo install --path /path/to/ralph-beads --bin ralph --force
ralph --version
```

- If you are working in this repo, you can also run:

```bash
make version
make verify-installed-version
```

## Project Config

Ralph supports per-project defaults in `.ralph/config.toml`:

```toml
max_iterations = 10
reflect_every = 3
capture_timeout_seconds = 30
capture_retries = 1
claude_timeout_minutes = 30
claude_retries = 1
close_guardrail_mode = "warn" # warn | strict
snapshot_consistency_enabled = false
```

CLI flags still take precedence over config values.
`close_guardrail_mode` validates that each issue-loop iteration closes only the active issue (`warn` by default, `strict` to stop after violating iterations). Reflection/cleanup passes are excluded.
Snapshot consistency checks are disabled by default unless enabled via `--snapshot-consistency` or `snapshot_consistency_enabled = true`.
Use `--skip-snapshot-consistency` to override and disable checks even if config enables them.

## Safety

Ralph uses `--dangerously-skip-permissions` for autonomous operation. Run only in trusted repositories and review all changes.
