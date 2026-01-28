# Claude Code Instructions

> See also: `AGENTS.md` for beads workflow details

## Project Overview

This is a **Ralph Loop** project - an automated development workflow that iteratively processes beads issues using Claude Code.

## Ralph Loop Context

When you are invoked by the Ralph loop (`ralph.sh`):
- You are processing a specific beads issue
- The issue details are provided at the end of your prompt
- Your job is to **fully implement** the issue requirements
- After successful implementation, **close the issue** with `bd close <issue-id>`

## Working on an Issue

1. **Understand** - Read the issue details carefully
2. **Plan** - Think through the implementation approach
3. **Implement** - Write the code, create files, make changes
4. **Test** - Verify your implementation works correctly
5. **Close** - Run `bd close <issue-id>` to mark complete

## Important Notes

- You are running with `--dangerously-skip-permissions` - use this power responsibly
- Each loop iteration processes ONE issue
- The loop continues automatically after you complete an issue
- If you cannot complete an issue, explain why in the output

## Code Standards

- Write clean, maintainable code
- Add appropriate comments for complex logic  
- Follow existing project conventions
- Test before marking issues complete
