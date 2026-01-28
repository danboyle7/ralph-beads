# Ralph-Beads Repository Guidelines

This repo contains **ralph-beads**, a tool for running autonomous Claude Code loops on beads issues. These guidelines are for working on this repo itself, not for the Ralph agent.

## Important Distinction

| File | Purpose |
|------|---------|
| `prompt.md` | Instructions for the **Ralph agent** when it works on a target project |
| `AGENTS.md` (this file) | Instructions for **you** when modifying this tool |

Do not confuse these. Changes to `prompt.md` affect how Ralph behaves in target projects. Changes here guide development of the tool itself.

## Architecture

```
ralph.sh          # Orchestration script - the loop runner
prompt.md         # Agent instructions (injected into each Claude call)
progress.txt      # Runtime log (auto-generated, gitignored)
archive/          # Previous run archives (auto-generated)
```

### How It Works

1. `ralph.sh` calls `bd ready` to get the next issue
2. Builds a prompt from `prompt.md` + issue details
3. Pipes to `claude --dangerously-skip-permissions --print`
4. Checks for `<promise>COMPLETE</promise>` signal
5. Repeats until done or max iterations reached

## Development Guidelines

### Testing Changes

Always test script changes with dry-run first:

```bash
./ralph.sh --dry-run
./ralph.sh --dry-run --verbose
```

### Modifying ralph.sh

- Keep the loop logic simple and predictable
- All Claude interaction goes through stdin pipe (no interactive mode)
- Exit codes: 0 = continue, 100 = all complete, other = error
- The `--dangerously-skip-permissions` flag is required for autonomous operation

### Modifying prompt.md

- This is the agent's "brain" - changes affect all target projects
- Keep instructions clear and unambiguous
- The `bd` command reference section is critical - keep it accurate
- Test prompt changes on a throwaway repo first

### Key Behaviors to Preserve

1. **Issue selection**: Always uses `bd ready` (priority-sorted, unblocked)
2. **Completion signal**: `<promise>COMPLETE</promise>` triggers clean exit
3. **Progress logging**: Append-only to `progress.txt`
4. **Archiving**: Previous runs archived before new run starts

## Dependencies

- `claude` CLI (Claude Code)
- `bd` CLI (Beads)
- `jq` for JSON parsing
- Standard bash utilities

## Common Tasks

### Adding a new CLI flag

1. Add to the `case` statement in argument parsing
2. Add to `--help` output
3. Document in README.md

### Changing the prompt format

The prompt is built in `build_prompt()`. It concatenates:
1. Base prompt from `prompt.md`
2. Current issue ID and details
3. Standard instructions (implement, test, close, signal complete)

### Debugging Claude interactions

Use `--verbose` to see issue details before each Claude call. The full prompt is visible in dry-run mode.
