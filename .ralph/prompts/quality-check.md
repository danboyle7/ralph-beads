# Reflection Pass: Quality Check

Audit code organization, cleanliness, and best practices.

## Goal
Produce actionable remediation work in beads before new feature work continues.

## Required Steps
1. Review repository structure and implementation quality using concrete evidence (file paths, commands, failing checks).
2. Keep only real findings (no speculative or style-only noise).
3. Create one remediation epic issue in beads (feature type is acceptable if epic type is unavailable).
4. For each finding, create a scoped beads issue linked to that epic.
5. Block currently open implementation issues on the remediation epic so work pauses until remediation is addressed.
6. Update `rules.md` with concise rules that prevent the detected recurring patterns.
7. Confirm all created issues/dependencies exist by re-checking `bd show`/`bd list`.

## Constraints
- Do not implement broad refactors in this pass.
- Focus on planning, issue creation, dependency wiring, and rule updates.
- Keep issue descriptions crisp and testable.
