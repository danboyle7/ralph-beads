# Security Policy

## Reporting a vulnerability

Please report potential security issues privately. Include:

- affected version/commit
- reproduction steps
- expected vs actual behavior
- impact assessment

Do not open public issues for unpatched vulnerabilities.

## Response targets

- Initial acknowledgment: within 3 business days
- Triage and severity assessment: as soon as practical
- Patch timeline: based on severity and exploitability

## Scope notes

This project orchestrates external CLIs (`claude`, `bd`) and can run with elevated autonomy (`--dangerously-skip-permissions`).
Security reports involving command execution boundaries and prompt-injection resilience are especially valuable.
