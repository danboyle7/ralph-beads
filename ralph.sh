#!/usr/bin/env bash
# Ralph Wiggum - Long-running AI agent loop using Claude Code + Beads
# Usage: ./ralph.sh [options] [max_iterations]

set -e

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPT_FILE="$SCRIPT_DIR/prompt.md"
PROGRESS_FILE="$SCRIPT_DIR/progress.txt"
ARCHIVE_DIR="$SCRIPT_DIR/archive"
LAST_RUN_FILE="$SCRIPT_DIR/.last-run"

# Defaults
MAX_ITERATIONS=10
DRY_RUN=false
ONCE=false
VERBOSE=false

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --once)
            ONCE=true
            MAX_ITERATIONS=1
            shift
            ;;
        --verbose|-v)
            VERBOSE=true
            shift
            ;;
        -h|--help)
            echo "Ralph Wiggum - Long-running AI agent loop"
            echo ""
            echo "Usage: ./ralph.sh [options] [max_iterations]"
            echo ""
            echo "Options:"
            echo "  --dry-run     Show what would be done without executing"
            echo "  --once        Run only one iteration (process single issue)"
            echo "  --verbose,-v  Enable verbose output"
            echo "  -h, --help    Show this help message"
            echo ""
            echo "Arguments:"
            echo "  max_iterations  Maximum number of iterations (default: 10)"
            echo ""
            echo "Environment variables:"
            echo "  RALPH_MAX_ITERATIONS  Override default max iterations"
            echo ""
            echo "Files:"
            echo "  prompt.md      Your instructions for Claude (required)"
            echo "  progress.txt   Log of Ralph's progress (auto-created)"
            echo "  archive/       Previous runs archived here"
            exit 0
            ;;
        *)
            # Assume it's max_iterations if it's a number
            if [[ "$1" =~ ^[0-9]+$ ]]; then
                MAX_ITERATIONS="$1"
            else
                echo -e "${RED}Unknown option: $1${NC}"
                echo "Use --help for usage information"
                exit 1
            fi
            shift
            ;;
    esac
done

# Allow environment variable override
MAX_ITERATIONS="${RALPH_MAX_ITERATIONS:-$MAX_ITERATIONS}"

log() {
    echo -e "${BLUE}[ralph]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[ralph]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[ralph]${NC} $1"
}

log_error() {
    echo -e "${RED}[ralph]${NC} $1"
}

log_progress() {
    local message="$1"
    echo -e "${CYAN}[ralph]${NC} $message"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $message" >> "$PROGRESS_FILE"
}

# Check prerequisites
check_prerequisites() {
    local missing=false

    if ! command -v claude &> /dev/null; then
        log_error "claude-code CLI not found. Please install it first."
        missing=true
    fi

    if ! command -v bd &> /dev/null; then
        log_error "beads (bd) CLI not found. Please install it first."
        missing=true
    fi

    if [[ ! -f "$PROMPT_FILE" ]]; then
        log_error "Prompt file not found: $PROMPT_FILE"
        log_error "Please create prompt.md with your instructions."
        missing=true
    fi

    if [[ "$missing" == "true" ]]; then
        exit 1
    fi
}

# Archive previous run if starting fresh
archive_previous_run() {
    if [[ ! -f "$PROGRESS_FILE" ]]; then
        return
    fi

    # Check if we should archive (file exists and has content beyond header)
    local line_count
    line_count=$(wc -l < "$PROGRESS_FILE" | tr -d ' ')

    if [[ $line_count -gt 3 ]]; then
        local last_run_id
        last_run_id=$(cat "$LAST_RUN_FILE" 2>/dev/null || echo "unknown")

        # Create archive folder
        local date_str
        date_str=$(date +%Y-%m-%d-%H%M%S)
        local archive_folder="$ARCHIVE_DIR/${date_str}-${last_run_id}"

        log "Archiving previous run: $last_run_id"
        mkdir -p "$archive_folder"
        cp "$PROGRESS_FILE" "$archive_folder/"

        # Also archive beads state snapshot
        if command -v bd &> /dev/null; then
            bd list --all > "$archive_folder/beads-snapshot.txt" 2>/dev/null || true
        fi

        log "   Archived to: $archive_folder"
    fi
}

# Initialize progress file for new run
init_progress_file() {
    local run_id
    run_id=$(date +%Y%m%d-%H%M%S)
    echo "$run_id" > "$LAST_RUN_FILE"

    cat > "$PROGRESS_FILE" << EOF
# Ralph Progress Log
Run ID: $run_id
Started: $(date)
Max Iterations: $MAX_ITERATIONS
---

EOF

    log "Progress file initialized: $PROGRESS_FILE"
}

# Get the next ready issue from beads
get_next_issue() {
    # Get the first ready (unblocked) issue
    local issue_id
    issue_id=$(bd ready --json 2>/dev/null | jq -r '.[0].id // empty' 2>/dev/null || true)

    if [[ -z "$issue_id" ]]; then
        # Fallback: try getting first open issue
        issue_id=$(bd list --status open --json 2>/dev/null | jq -r '.[0].id // empty' 2>/dev/null || true)
    fi

    echo "$issue_id"
}

# Get issue details
get_issue_details() {
    local issue_id="$1"
    bd show "$issue_id" 2>/dev/null || echo "Issue: $issue_id"
}

# Get issue count
get_open_issue_count() {
    bd list --status open --json 2>/dev/null | jq -r 'length' 2>/dev/null || echo "?"
}

# Build the prompt for Claude
build_prompt() {
    local issue_id="$1"
    local issue_details="$2"
    local base_prompt

    base_prompt=$(cat "$PROMPT_FILE")

    cat <<EOF
${base_prompt}

---

## Current Issue

Issue ID: ${issue_id}

${issue_details}

---

## Instructions

1. Implement what this issue requires
2. Test your implementation
3. When complete, close the issue: \`bd close ${issue_id}\`
4. If ALL issues are now complete, output: <promise>COMPLETE</promise>

EOF
}

# Run Claude Code on an issue
run_claude() {
    local issue_id="$1"
    local prompt="$2"
    local output

    if [[ "$DRY_RUN" == "true" ]]; then
        log "[DRY RUN] Would run claude-code on issue: $issue_id"
        log "[DRY RUN] Prompt preview:"
        echo "$prompt" | head -30
        echo "..."
        return 0
    fi

    log "Running Claude Code on issue: $issue_id"

    # Run claude-code with permissions bypassed for autonomous operation
    # tee to stderr so we see output in real-time while capturing it
    output=$(echo "$prompt" | claude \
        --dangerously-skip-permissions \
        --print \
        - 2>&1 | tee /dev/stderr) || true

    # Check for completion signal
    if echo "$output" | grep -q "<promise>COMPLETE</promise>"; then
        return 100  # Special exit code for "all done"
    fi

    return 0
}

# Main loop
main() {
    echo ""
    echo -e "${CYAN}╔═══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║                      Ralph Wiggum                             ║${NC}"
    echo -e "${CYAN}║           Long-running AI Agent Loop (Beads Edition)          ║${NC}"
    echo -e "${CYAN}╚═══════════════════════════════════════════════════════════════╝${NC}"
    echo ""

    check_prerequisites

    # Archive previous run and start fresh
    archive_previous_run
    init_progress_file

    local open_count
    open_count=$(get_open_issue_count)

    log "Prompt file: $PROMPT_FILE"
    log "Open issues: $open_count"
    log "Max iterations: $MAX_ITERATIONS"
    echo ""

    log_progress "Starting Ralph loop with $open_count open issues"

    local issues_processed=0

    for i in $(seq 1 $MAX_ITERATIONS); do
        echo ""
        echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
        echo -e "${BLUE}  Ralph Iteration $i of $MAX_ITERATIONS${NC}"
        echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
        echo ""

        # Get next issue
        local issue_id
        issue_id=$(get_next_issue)

        if [[ -z "$issue_id" ]]; then
            log_success "No more issues to process. All done!"
            log_progress "COMPLETE: No more issues to process"
            echo ""
            echo -e "${GREEN}Ralph completed all tasks!${NC}"
            echo "Completed at iteration $i of $MAX_ITERATIONS"

            # Sync beads at the end
            log "Syncing beads..."
            bd sync 2>/dev/null || true

            exit 0
        fi

        log "Found issue: $issue_id"
        log_progress "Iteration $i: Processing issue $issue_id"

        # Get issue details
        local issue_details
        issue_details=$(get_issue_details "$issue_id")

        if [[ "$VERBOSE" == "true" ]]; then
            log "Issue details:"
            echo "$issue_details"
            echo ""
        fi

        # Build and run prompt
        local prompt
        prompt=$(build_prompt "$issue_id" "$issue_details")

        local claude_exit=0
        run_claude "$issue_id" "$prompt" || claude_exit=$?

        if [[ $claude_exit -eq 100 ]]; then
            # Received completion signal
            log_success "Received completion signal from Claude"
            log_progress "COMPLETE: All issues done (signaled by Claude)"
            ((issues_processed++))
            echo ""
            echo -e "${GREEN}Ralph completed all tasks!${NC}"
            echo "Completed at iteration $i of $MAX_ITERATIONS"

            # Sync beads at the end
            log "Syncing beads..."
            bd sync 2>/dev/null || true

            exit 0
        elif [[ $claude_exit -eq 0 ]]; then
            log_success "Completed iteration for issue: $issue_id"
            log_progress "Iteration $i: Completed issue $issue_id"
            ((issues_processed++))
        else
            log_error "Failed to process issue: $issue_id (exit code: $claude_exit)"
            log_progress "Iteration $i: FAILED issue $issue_id (exit: $claude_exit)"
            log_warn "Continuing to next issue..."
        fi

        # Exit after one iteration if --once flag is set
        if [[ "$ONCE" == "true" ]]; then
            log "Single iteration mode. Stopping."
            break
        fi

        echo ""
        log "Iteration $i complete. Continuing..."
        sleep 2
    done

    echo ""
    log_warn "Ralph reached max iterations ($MAX_ITERATIONS) without completing all tasks."
    log_progress "STOPPED: Reached max iterations ($MAX_ITERATIONS)"
    log "Check $PROGRESS_FILE for status."

    # Sync beads at the end
    log "Syncing beads..."
    bd sync 2>/dev/null || true

    exit 1
}

main "$@"
