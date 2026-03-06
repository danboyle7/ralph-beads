---
name: Feature Request
about: Propose a new feature or enhancement for the ralph-beads tool repo
title: "[Feature] "
labels: ["enhancement", "triage"]
assignees: []
---

## Summary

What feature do you want to add or improve?

## Problem / Motivation

Why is this feature needed?

## Proposed Change

Describe the expected behavior and implementation direction.

## Scope

- [ ] CLI/runtime behavior
- [ ] Prompt templates (`prompts/*.md`)
- [ ] Docs/policies (`README.md`, `.github/*.md`, `LICENSE`)
- [ ] Other (describe below)

## Acceptance Criteria

List clear, testable outcomes.

## Validation Plan

Describe how this should be validated.

```bash
cargo check
```

If prompt templates are changed, also validate in a throwaway Beads repo:

```bash
cargo run --bin ralph -- --dry-run
cargo run --bin ralph -- --dry-run --verbose
```

## Additional Context

Links, screenshots, logs, or related issues.
