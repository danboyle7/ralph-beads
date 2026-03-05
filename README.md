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

# Print last run summary
ralph summary
```

## Cleanup Behavior

- `cleanup` runs a dedicated cleanup prompt pass (`.ralph/prompts/cleanup.md`) and exits.
- Normal loop runs now auto-detect interrupted in-progress work from the previous run log and executes one cleanup pass before continuing.

## Reflection Behavior

- `reflect` runs two passes and exits:
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
├── prompt.md (legacy compatibility template)
├── cleanup.md
├── quality-check.md
├── code-review-check.md
├── validation-check.md
└── src/
```

Project layout after `ralph init`:

```text
your-project/
└── .ralph/
    ├── prompts/
    │   ├── ralph.md
    │   ├── issue.md
    │   ├── cleanup.md
    │   ├── quality-check.md
    │   ├── code-review-check.md
    │   └── validation-check.md
    ├── progress.txt
    ├── archive/
    ├── logs/
    └── .last-run
```

## Progress Tracking

Ralph automatically:
- Appends progress to `.ralph/progress.txt`
- Archives previous run logs in `.ralph/archive/`
- Writes debug logs in `.ralph/logs/` when `--debug` is enabled

Add to project `.gitignore`:

```gitignore
.ralph/progress.txt
.ralph/archive/
.ralph/.last-run
.ralph/logs/
```

## Environment Variables

- `RALPH_MAX_ITERATIONS` overrides the default iteration budget.

## Safety

Ralph uses `--dangerously-skip-permissions` for autonomous operation. Run only in trusted repositories and review all changes.
