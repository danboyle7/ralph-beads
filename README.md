# Ralph Beads

An automated development workflow that uses **Claude Code** to iteratively process **Beads** issues.

## Overview

Ralph Wiggum is a long-running AI agent loop that:
1. Reads the next ready issue from Beads
2. Sends it to Claude Code with your prompt
3. Claude implements the solution and closes the issue
4. Repeats until all issues are done (or signals `<promise>COMPLETE</promise>`)

This replaces traditional PRD-based workflows with Beads issue tracking.

## Prerequisites

- [Claude Code CLI](https://docs.anthropic.com/claude-code) installed and authenticated
- [Beads (bd)](https://github.com/...) installed
- `jq` for JSON parsing

## Installation

```bash
# Clone this repo
git clone https://github.com/youruser/ralph-beads.git

# Build + install the Rust CLI
cargo install --path /path/to/ralph-beads --bin ralph --force
```

## Setup (per project)

```bash
# 1. Initialize Ralph (also bootstraps beads if needed)
cd your-project
ralph --init

# 2. Edit the prompt with your project-specific instructions
$EDITOR .ralph/prompt.md

# 3. Create your issues using Claude in plan mode
claude --permission-mode plan
bd create "Issue title" --type task --description "What needs to be done..."

# 4. Run Ralph
ralph
```

## Usage

```bash
# Run with default 10 iterations
ralph

# Run with custom max iterations
ralph --iterations 20

# Process only one issue
ralph --once

# See what would happen without executing
ralph --dry-run

# Verbose output
ralph --verbose

## Workflow

### Phase 1: Planning (Manual)
Use Claude in plan mode to:
- Understand the project requirements
- Break down work into discrete issues
- Create all beads stories/issues upfront

```bash
# Plan your work
claude --permission-mode plan

# Create issues from your plan
bd create "Implement user authentication" --type feature --description "..."
bd create "Add database migrations" --type task --description "..."
bd create "Write API tests" --type task --description "..."
```

### Phase 2: Execution (Automated)
Run the Ralph loop to process issues:

```bash
ralph
```

The loop will:
1. Find the next unblocked issue (`bd ready`)
2. Build a prompt combining `.ralph/prompt.md` + issue details
3. Call Claude Code with `--dangerously-skip-permissions`
4. Claude implements and closes the issue
5. Check for completion signal (`<promise>COMPLETE</promise>`)
6. Move to next issue or exit if complete

## Files

**This repo (ralph-beads):**
```
ralph-beads/
├── src/main.rs        # The main Ralph Rust CLI
├── prompt.md          # Default prompt template (copied on --init)
├── CLAUDE.md          # Points to AGENTS.md
└── AGENTS.md          # Guidelines for developing/maintaining this repo
```

**Your project (after `ralph --init`):**
```
your-project/
├── .ralph/
│   ├── prompt.md      # Your project-specific instructions
│   ├── progress.txt   # Log of Ralph's progress (auto-created)
│   ├── archive/       # Previous runs archived here
│   └── .last-run      # Run tracking
└── ...
```

## Progress Tracking

Ralph automatically:
- Creates `.ralph/progress.txt` to log each iteration
- Archives previous runs to `.ralph/archive/` when starting fresh
- Saves beads snapshots with each archive

**Add to your project's `.gitignore`:**
```gitignore
# Ralph state files (keep prompt.md, ignore the rest)
.ralph/progress.txt
.ralph/archive/
.ralph/.last-run
```

## Completion Signal

Claude can signal completion by outputting:
```
<promise>COMPLETE</promise>
```

This tells Ralph that all work is done, even if issues remain (e.g., future work items).

## Environment Variables

- `RALPH_MAX_ITERATIONS` - Override default max iterations (default: 10)

## Safety Notes

This script uses `--dangerously-skip-permissions` to allow Claude to work autonomously. Only run this:
- In trusted, sandboxed environments
- On code you're prepared to review
- With proper backups/version control

## Beads Commands Reference

```bash
bd ready              # Find unblocked work
bd list               # List all issues  
bd list --status open # List open issues
bd show <id>          # Show issue details
bd create "Title"     # Create new issue
bd close <id>         # Close an issue
bd sync               # Sync with git
```
