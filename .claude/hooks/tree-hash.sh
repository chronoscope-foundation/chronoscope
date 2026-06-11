#!/usr/bin/env bash
# Stable hash of HEAD + uncommitted changes. Sourced by both `just check`
# (writes the marker) and the pre-commit hook (verifies it). One definition
# so the two can never drift on input format.
set -uo pipefail

PROJECT_DIR="${1:-${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}}"

if command -v sha256sum >/dev/null 2>&1; then
    hash_cmd() { sha256sum; }
else
    hash_cmd() { shasum -a 256; }
fi

{
    git -C "$PROJECT_DIR" rev-parse HEAD 2>/dev/null || echo no-head
    git -C "$PROJECT_DIR" diff HEAD 2>/dev/null
    # Paths only: staging flips porcelain's XY columns without changing
    # content, and add→commit between check and commit is the normal flow.
    # Content is covered by `git diff HEAD`; untracked files count by path.
    git -C "$PROJECT_DIR" status --porcelain 2>/dev/null | cut -c4-
} | hash_cmd | cut -d' ' -f1
