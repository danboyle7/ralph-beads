# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/danboyle7/ralph-beads/releases/tag/v0.1.1) - 2026-03-06

### Added

- add release-plz with ci-backed releases
- add github ci workflows
- *(reflection)* add code-review pass and improve live run UI
- extra features -- remove shell version
- *(summary)* add cross-run intelligence metrics and playbook suggestions
- *(semantic)* add canonical event graph and live timeline/validation/subagent surfaces
- *(summaries)* strip ansi/control noise from surfaced log summaries
- *(correlation)* keep short UI ids and log full tool ids in semantic records
- *(logs)* add semantic ndjson and curated markdown run report
- *(output)* emit evidence-first execution narrative
- *(activity)* add phase transitions and validation retry causality
- *(logs)* track tool lifecycle and emit tool completion activity
- add rust version, shell version worked but it was limited
- add ralph prompt, update CLAUDE.md and AGENTS.md to reflect repo management
- initial commit

### Fixed

- fix formatting issues
- fix clippy lint issues
- fix preflight check issues with beads. need to use --flat to ensure json payload is correctly formatted
- removed beads, it was only needed for prompt initialization, nothing more

### Other

- update SECURITY.md with dangers of claude without permission guards
