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

## High-risk mode: `--dangerously-skip-permissions`

Ralph's autonomous loop uses `--dangerously-skip-permissions`. This bypasses permission prompts and should be treated like running an automated shell agent with your user privileges.

## Risks in this mode

- Destructive local actions (edits/deletes/history rewrite).
- Secret exposure from env/config/files.
- Prompt-injection-driven unsafe commands.
- Untrusted toolchain/build-script execution.
- Possible data exfiltration if network is available.

## Safe operating baseline

- Only run in trusted repositories you control.
- Use a low-privilege environment (separate user/container/VM).
- Do not run with production credentials loaded; prefer short-lived scoped tokens.
- Review all diffs before commit/push and protect main with required review + CI.
- Prefer restricted network/filesystem access where possible.

## If you suspect compromise

- Revoke and rotate any credentials present in the execution environment.
- Inspect shell history, recent commits, and changed files for unauthorized actions.
- Preserve artifacts/logs needed for incident analysis.
- Report the issue privately with reproduction details and impact.
