#!/usr/bin/env bash
# PreToolUse hook on Bash: when Claude runs `git commit`, prompt for
# confirmation unless the full `just check` (target "all") has passed against
# the current tree. It asks rather than hard-blocks, so a deliberate WIP
# checkpoint can still go through — but skipping the full gate is a conscious
# choice, never a silent one.

set -uo pipefail

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$(pwd)}"
MARKER="$PROJECT_DIR/.claude/last-check.json"

# Bail out for non-git-commit commands using bash builtins only — this hook
# fires on every Bash tool call, so avoid forking grep/sed in the hot path.
INPUT="$(cat)"
case "$INPUT" in
    *'"command":"git commit'*) ;;
    *'"command": "git commit'*) ;;
    *) exit 0 ;;
esac

# Emit an "ask" decision (Claude Code prompts the user to confirm) and exit.
ask() {
    local reason="$1"
    cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "ask",
    "permissionDecisionReason": "$reason"
  }
}
EOF
    exit 0
}

if [ ! -f "$MARKER" ]; then
    ask "No .claude/last-check.json — the full 'just check' has not been run in this checkout (or the marker was cleared). 'just check' (no target) is the hermetic ground-truth gate. Run it before committing, or confirm to commit anyway."
fi

# Read marker once; parse with bash regex. Field order in the JSON is
# irrelevant; missing fields leave the variables unset (handled below).
MARKER_BODY="$(cat "$MARKER" 2>/dev/null)" || MARKER_BODY=""
RECORDED=""
TARGET=""
TIMESTAMP=""
[[ "$MARKER_BODY" =~ \"hash\"[[:space:]]*:[[:space:]]*\"([^\"]+)\" ]] && RECORDED="${BASH_REMATCH[1]}"
[[ "$MARKER_BODY" =~ \"target\"[[:space:]]*:[[:space:]]*\"([^\"]+)\" ]] && TARGET="${BASH_REMATCH[1]}"
[[ "$MARKER_BODY" =~ \"timestamp\"[[:space:]]*:[[:space:]]*\"([^\"]+)\" ]] && TIMESTAMP="${BASH_REMATCH[1]}"

CURRENT=$("$PROJECT_DIR/.claude/hooks/tree-hash.sh" "$PROJECT_DIR")

if [ "$CURRENT" != "$RECORDED" ]; then
    ask "Working tree has changed since the last successful 'just check' (target: ${TARGET:-unknown}, at ${TIMESTAMP:-unknown}). Run 'just check' so the gate is confirmed against this exact tree, or confirm to commit anyway."
fi

if [ "$TARGET" != "all" ]; then
    ask "The last 'just check' against this tree was the scoped '${TARGET:-unknown}' subset, not the full gate — some checks (notably the browser suite, web-test) run only in the full 'just check' (no target). Run 'just check' before committing, or confirm to commit anyway."
fi

# Full gate confirmed against the current tree — allow silently.
exit 0
