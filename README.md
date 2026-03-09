# Ralph Beads

`ralph` is a Rust CLI that runs autonomous Claude Code loops on Beads issues.

## How it works

1. Fetches the next ready non-epic issue via `bd ready`
   - If no non-epic issue is ready but Beads still has non-closed work, Ralph runs a repair pass first by default.
2. Builds a prompt from:
   - `.ralph/prompts/ralph.md` (shared rules)
   - `.ralph/prompts/issue.md` (issue mode)
   - runtime issue/rules/progress context
3. Runs `claude --dangerously-skip-permissions --print`
4. Detects completion (`<promise>COMPLETE</promise>`) or continues to the next issue

## Repository layout

```text
ralph-beads/
├── AGENTS.md
├── CLAUDE.md
├── docs/
│   └── TODO.md
├── release-plz.toml
├── prompts/
│   ├── ralph.md
│   ├── issue.md
│   ├── cleanup.md
│   ├── repair.md
│   ├── quality-check.md
│   ├── code-review-check.md
│   └── validation-check.md
├── src/
└── .github/
    ├── CONTRIBUTING.md
    ├── CODE_OF_CONDUCT.md
    ├── SECURITY.md
    └── workflows/
        ├── ci.yml
        ├── dependency-audit.yml
        └── release-plz.yml
```

## Prerequisites

- [Claude Code CLI](https://docs.anthropic.com/claude-code)
- `bd` (Beads)
- Rust toolchain

## Beads setup (`bd` + Dolt)

- Beads repo: [steveyegge/beads](https://github.com/steveyegge/beads)
- Beads install/setup docs: [Installation](https://steveyegge.github.io/beads/getting-started/installation/) and [Quick Start](https://steveyegge.github.io/beads/getting-started/quickstart/)
- Dolt backend docs (used by Beads): [What Is Dolt?](https://docs.dolthub.com/introduction/what-is-dolt)

## Install

```bash
cargo install --path . --bin ralph --force
```

## Project setup (target repo)

```bash
cd your-project
ralph init
$EDITOR .ralph/prompts/issue.md
$EDITOR AGENTS.md
```

Behavior notes:
- `ralph init` auto-runs `bd init` only when `.beads` is missing.
- If `.beads` already exists, `ralph init` leaves it unchanged.
- If `.ralph` already exists, `ralph init` exits with an error and does not overwrite existing Ralph files.
- `ralph doctor` and `ralph upgrade-prompts` also ensure `.beads` exists (they run `bd init` only if needed).
- Normal loop commands (`ralph`, `ralph --dry-run`, etc.) do not auto-initialize Beads; they fail fast if `.beads` is missing.

## Usage

```bash
# Default loop (10 iterations)
ralph

# Custom iteration budget
ralph --iterations 20

# Single issue
ralph --once

# Prompt dry-run
ralph --dry-run
# (prints prompts only; skips close-guardrail verification)

# Enable/disable issue snapshot consistency checks
ralph --snapshot-consistency
ralph --skip-snapshot-consistency

# Disable the automatic repair pass when nothing is ready
ralph --no-repair

# Verbose output
ralph --verbose

# One-off passes
ralph cleanup
ralph reflect

# Run reflection suite every N iterations
ralph --reflect-every 3

# Run reflection suite when an epic issue is closed
ralph --reflect-every-epic

# Project health/layout checks
ralph doctor
ralph preflight

# Upgrade prompt templates in target project
ralph upgrade-prompts

# Last run summary
ralph summary
ralph summary --json

# Version
ralph --version
```

Interactive TUI note:
- While a loop is actively running, press `n` to queue one more iteration or `x` to open the same numeric prompt and add a custom amount. Ralph applies the queued increase at the next iteration boundary.
- When Ralph reaches the current iteration budget without finishing, the TUI can extend the same run in place.
- Press `n` for one more iteration, `x` to open a numeric prompt (prefilled with `5`) and add that many more iterations, or `r` to run the reflection suite without leaving the TUI.
- When a run finishes or there is no ready work to do, press `r` to run the reflection suite from the finished TUI state.
- Press `1`, `2`, `3`, and `4` to toggle the output, diff, side, and terminal columns on or off. The terminal column stays on the far right when enabled.
- Press `t` as a shortcut for the terminal column, click that pane or press `Ctrl+T` to focus it, and press `F12` to close the embedded terminal.
- These controls only appear after the current budget is exhausted; plain mode still stops at the configured budget.

Issue selection note:
- Ralph preserves `bd ready` ordering but skips items typed as `epic`; each loop iteration executes a single ready child issue/work item.
- In issue mode, the runtime preselects that single issue for the current invocation. `bd ready` may be used to confirm queue state, but not to switch to a second issue mid-run.
- If no non-epic issue is ready but non-closed work remains, Ralph runs `.ralph/prompts/repair.md` once and then re-checks `bd ready`.

## Cleanup + repair + reflection

- `ralph cleanup` runs `.ralph/prompts/cleanup.md` once, then exits.
- The main loop runs `.ralph/prompts/repair.md` automatically when `bd ready` is empty but non-closed work remains, unless `--no-repair` is set or `auto_repair_enabled = false`.
- `ralph reflect` runs all reflection prompts once, then exits:
  - `.ralph/prompts/quality-check.md`
  - `.ralph/prompts/code-review-check.md`
  - `.ralph/prompts/validation-check.md`

## Files created in target projects

```text
your-project/
├── AGENTS.md
└── .ralph/
    ├── prompts/
    │   ├── ralph.md
    │   ├── issue.md
    │   ├── cleanup.md
    │   ├── repair.md
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

`ralph init` and `ralph doctor` scaffold `AGENTS.md` if missing.

## Local development

```bash
make check
cargo run --bin ralph -- --dry-run
cargo run --bin ralph -- --dry-run --verbose
```

## Release automation

This repo uses `release-plz` with:
- [release-plz.toml](./release-plz.toml) (`git_only = true`)
- [release workflow](./.github/workflows/release-plz.yml)

On push to `main`/`master`:
- `release-pr` opens or updates a release PR with version/changelog changes.
- `release` creates git tags and GitHub releases when a release PR merge commit is on the default branch.

Required GitHub repo settings:
- `Settings -> Actions -> General -> Workflow permissions`: `Read and write permissions`
- Enable `Allow GitHub Actions to create and approve pull requests`

If you later want crates.io publishing:
- set `git_only = false` in `release-plz.toml`
- add `CARGO_REGISTRY_TOKEN` repository secret

## Config

Per-project config is read from `.ralph/config.toml`:

```toml
max_iterations = 10
reflect_every = 3
reflect_every_epic = false
auto_repair_enabled = true
capture_timeout_seconds = 30
capture_retries = 1
claude_timeout_minutes = 30
claude_retries = 1
terminal_scrollback_lines = 10000
close_guardrail_mode = "warn" # warn | strict
snapshot_consistency_enabled = false
```

CLI flags override config values.

## Safety

Ralph uses `--dangerously-skip-permissions` for autonomous operation. Only run in trusted repos.

## License

This project is released under the **Apache License 2.0**.

## Contributing and policies

- Contribution guide: [`.github/CONTRIBUTING.md`](./.github/CONTRIBUTING.md)
- Code of conduct: [`.github/CODE_OF_CONDUCT.md`](./.github/CODE_OF_CONDUCT.md)
- Security policy: [`.github/SECURITY.md`](./.github/SECURITY.md)
