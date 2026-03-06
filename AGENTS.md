# Ralph-Beads Agent Guidelines

This file is for agents modifying the **ralph-beads tool repo** itself.

## Scope And Distinction

- `prompts/*.md` defines Ralph behavior in downstream target repos.
- `AGENTS.md` defines how to safely change this tool repo.
- Treat prompt edits as high-impact: they affect every project using `ralph init`.

## Minimal Orientation

- CLI/runtime entry and orchestration: `src/main.rs`, `src/runner.rs`
- Prompt assembly: `src/prompts.rs`
- Project bootstrap/repair/upgrade: `src/init.rs`
- CLI flags and path layout: `src/cli.rs`
- Snapshot/issue integrity checks: `src/issues.rs`
- Run lock + persisted run state: `src/run_state.rs`
- User docs/policies: `README.md`, `.github/*.md`, `LICENSE`

## Non-Negotiable Invariants

1. Issue selection must come from `bd ready` (ready/unblocked ordering).
2. Completion signal remains `<promise>COMPLETE</promise>`.
3. Progress log behavior remains append-only.
4. Previous run artifacts are archived before a new run starts.
5. Claude execution remains non-interactive (stdin-driven).
6. Autonomous execution keeps `--dangerously-skip-permissions`.
7. Exit codes stay stable: `0` continue/success path, `100` all complete, other values for errors.

## Required Workflow For Changes

1. Make the smallest change that preserves invariants.
2. Update docs when behavior/flags/layout change.
3. Validate locally:
   - `cargo check`
4. Validate loop behavior in a throwaway Beads repo (not this repo):
   - `cargo run --bin ralph -- --dry-run`
   - `cargo run --bin ralph -- --dry-run --verbose`

## Prompt-Change Workflow (High Impact)

When editing `prompts/*.md`:

1. Keep instructions explicit and testable.
2. Keep `bd` command usage accurate.
3. Avoid ambiguous language that could widen execution scope.
4. Run throwaway-repo dry-run checks before merge.

## Things To Avoid

- Do not silently change loop semantics while making refactors.
- Do not couple unrelated behavior changes in one PR.
- Do not introduce interactive Claude flows.
- Do not remove or weaken issue-close guardrails without explicit rationale in docs/PR notes.

## Common Update Patterns

### Adding a new CLI flag

1. Add the flag to `src/cli.rs`.
2. Apply behavior in `src/main.rs` / `src/settings.rs` / relevant module.
3. Document the flag and examples in `README.md`.

### Changing prompt composition

Prompt composition lives in `src/prompts.rs`:

1. `build_issue_prompt()`
2. `build_cleanup_prompt()`
3. `build_reflection_prompt()`

Ensure shared prompt + mode prompt + runtime context remain clearly separated.
