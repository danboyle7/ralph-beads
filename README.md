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

## Setup

1. **Create your issues first** using Claude in plan mode:
   ```bash
   # Open Claude in plan mode to design your issues
   claude --permission-mode plan
   
   # Then create issues based on your planning session
   bd create "Issue title" --type task --description "What needs to be done..."
   ```

2. **Customize the prompt** by editing `prompt.md` with your specific instructions

3. **Run the loop**:
   ```bash
   ./ralph.sh
   ```

## Usage

```bash
# Run with default 10 iterations
./ralph.sh

# Run with custom max iterations
./ralph.sh 20

# Process only one issue
./ralph.sh --once

# See what would happen without executing
./ralph.sh --dry-run

# Verbose output
./ralph.sh --verbose
```

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
./ralph.sh
```

The loop will:
1. Find the next unblocked issue (`bd ready`)
2. Build a prompt combining `prompt.md` + issue details
3. Call Claude Code with `--dangerously-skip-permissions`
4. Claude implements and closes the issue
5. Check for completion signal (`<promise>COMPLETE</promise>`)
6. Move to next issue or exit if complete

## Files

```
ralph-beads/
├── ralph.sh           # The main Ralph loop script
├── prompt.md          # Your custom prompt for Claude (you create this)
├── progress.txt       # Log of Ralph's progress (auto-created)
├── archive/           # Previous runs archived here
├── CLAUDE.md          # Instructions for Claude Code sessions
├── AGENTS.md          # Beads workflow instructions
└── .beads/            # Beads database
```

## Progress Tracking

Ralph automatically:
- Creates `progress.txt` to log each iteration
- Archives previous runs to `archive/` when starting fresh
- Saves beads snapshots with each archive

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
