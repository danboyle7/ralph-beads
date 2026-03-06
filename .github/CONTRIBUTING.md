# Contributing

## Scope

This repository is the `ralph` tool itself. Prompt template changes in `prompts/` affect behavior in every target project.

## Local setup

1. Install Rust toolchain
2. Ensure `bd` and `claude` CLIs are available
3. Build/check from repo root:

```bash
make check
cargo run --bin ralph -- --version
```

## Required validation

Before opening a PR, run:

```bash
make check
cargo run --bin ralph -- --dry-run
cargo run --bin ralph -- --dry-run --verbose
```

If your change touches prompt templates (`prompts/*.md`), test on a throwaway repo after `ralph init`.

PRs also run GitHub Actions for formatting, clippy, tests, cross-platform build checks, and dependency audit.
Default-branch pushes also run `release-plz` to open/update release PRs and cut tags/releases after release PR merges.

## Code change guidelines

- Keep loop behavior deterministic and explicit.
- Preserve exit semantics: `0` continue, `100` complete, other = error.
- Keep Claude execution non-interactive via stdin.
- Do not remove `--dangerously-skip-permissions` from autonomous runs.

## Prompt template guidelines

- Keep instructions specific and unambiguous.
- Keep command references accurate (`bd`, `git`, validation flow).
- Avoid hidden behavioral changes; document intent in PR description.

## Docs

Update `README.md` and `AGENTS.md` when changing CLI behavior, project layout, or default templates.
