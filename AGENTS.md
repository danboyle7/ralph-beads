# Ralph-Beads Repository Guidelines

This repo contains **ralph-beads**, a tool for running autonomous Claude Code loops on beads issues. These guidelines are for working on this repo itself, not for the Ralph agent.

## Important Distinction

| File | Purpose |
|------|---------|
| `ralph.md` + `issue.md` | Shared and issue-mode instructions for the **Ralph agent** |
| `AGENTS.md` (this file) | Instructions for **you** when modifying this tool |

Do not confuse these. Changes to agent prompt files affect how Ralph behaves in target projects. Changes here guide development of the tool itself.

## Architecture

```
src/main.rs       # Rust CLI entrypoint
src/cli.rs        # CLI args and path layout
src/init.rs       # `ralph init` bootstrap logic
src/summary.rs    # `ralph summary` rendering
ralph.md          # Shared meta prompt template
issue.md          # Issue-mode prompt template
prompt.md         # Legacy compatibility prompt template
cleanup.md        # Default cleanup pass template
quality-check.md  # Default reflection quality template
code-review-check.md # Default reflection code-review template
validation-check.md # Default reflection validation template
.ralph/progress.txt # Runtime log (in target project, auto-generated)
.ralph/archive/   # Previous run archives (in target project, auto-generated)
```

### How It Works

1. `ralph` calls `bd ready` to get the next issue
2. Builds prompts from `.ralph/prompts/*.md` + runtime context
3. Pipes to `claude --dangerously-skip-permissions --print`
4. Checks for `<promise>COMPLETE</promise>` signal
5. Repeats until done or max iterations reached

## Development Guidelines

### Testing Changes

Always test loop changes with dry-run first:

```bash
cargo run --bin ralph -- --dry-run
cargo run --bin ralph -- --dry-run --verbose
```

### Modifying the Rust CLI

- Keep the loop logic simple and predictable
- All Claude interaction goes through stdin pipe (no interactive mode)
- Exit codes: 0 = continue, 100 = all complete, other = error
- The `--dangerously-skip-permissions` flag is always used for autonomous operation

### Modifying prompt templates

- These files are the agent's "brain" - changes affect all target projects
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
- Rust toolchain

## Common Tasks

### Adding a new CLI flag

1. Add to `src/cli.rs` (`Cli` struct)
2. Wire behavior in `src/main.rs` (or a submodule)
3. Document in README.md

### Changing the prompt format

The main issue prompt is built in `build_prompt()`. It concatenates:
1. Shared prompt from `.ralph/prompts/ralph.md`
2. Issue-mode prompt from `.ralph/prompts/issue.md` (legacy fallbacks supported)
3. Runtime context sections (issue details, rules.md, progress log, and instructions)

### Debugging Claude interactions

Use `--verbose` to see issue details before each Claude call. The full prompt is visible in dry-run mode.
