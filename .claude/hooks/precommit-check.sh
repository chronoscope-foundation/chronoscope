#!/usr/bin/env bash
# PreToolUse hook on Bash: when Claude runs `git commit`, advise re-running
# `just check` if the working tree has drifted from the last successful run.
# Always exit 0 — advisory, never blocking.

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

advise() {
    local message="$1"
    cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "additionalContext": "$message"
  }
}
EOF
}

if [ ! -f "$MARKER" ]; then
    advise "Note: no .claude/last-check.json was found — 'just check' has not been run in this checkout (or the marker was cleared). Consider running 'just check' before committing; it is the hermetic ground-truth gate."
    exit 0
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
if [ "$CURRENT" = "$RECORDED" ]; then
    exit 0
fi

advise "Note: working tree has changed since the last successful 'just check' (target: ${TARGET:-unknown}, at ${TIMESTAMP:-unknown}). Consider running 'just check' before committing — it is the hermetic ground-truth gate, and the marker is what confirms the gate passed against this exact tree state."
exit 0
