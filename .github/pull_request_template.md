## Summary

Describe what this PR changes and why.

## Type Of Change

- [ ] Bug fix
- [ ] Feature
- [ ] Refactor
- [ ] Docs
- [ ] Prompt template update (`prompts/*.md`)
- [ ] Other (describe below)

## Linked Issues

Closes #

## Validation

Describe what you ran and results.

```bash
cargo check
```

If prompt templates were changed, also include throwaway-repo dry-run checks:

```bash
cargo run --bin ralph -- --dry-run
cargo run --bin ralph -- --dry-run --verbose
```

## Behavior / Invariant Checks

- [ ] Issue selection still comes from `bd ready`
- [ ] Completion signal remains `<promise>COMPLETE</promise>`
- [ ] Progress log behavior remains append-only
- [ ] Previous run artifacts are archived before a new run starts
- [ ] Claude execution remains non-interactive (stdin-driven)
- [ ] Autonomous execution keeps `--dangerously-skip-permissions`
- [ ] Exit code semantics remain stable (`0`, `100`, errors)

## Breaking Changes

- [ ] None
- [ ] Yes (describe below)

## Notes

Any extra context for reviewers.
