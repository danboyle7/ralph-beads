# Ralph Beads

`ralph` is a Rust CLI that runs autonomous Claude Code loops on Beads issues.

## How it works

1. Fetches the next ready issue via `bd ready`
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
├── prompts/
│   ├── ralph.md
│   ├── issue.md
│   ├── cleanup.md
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
        └── dependency-audit.yml
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

# Verbose output
ralph --verbose

# One-off passes
ralph cleanup
ralph reflect

# Run reflection suite every N iterations
ralph --reflect-every 3

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

## Cleanup + reflection

- `ralph cleanup` runs `.ralph/prompts/cleanup.md` once, then exits.
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

## Config

Per-project config is read from `.ralph/config.toml`:

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

CLI flags override config values.

## Safety

Ralph uses `--dangerously-skip-permissions` for autonomous operation. Only run in trusted repos.

## License

This project is released under the **Apache License 2.0**.

## Contributing and policies

- Contribution guide: [`.github/CONTRIBUTING.md`](./.github/CONTRIBUTING.md)
- Code of conduct: [`.github/CODE_OF_CONDUCT.md`](./.github/CODE_OF_CONDUCT.md)
- Security policy: [`.github/SECURITY.md`](./.github/SECURITY.md)
