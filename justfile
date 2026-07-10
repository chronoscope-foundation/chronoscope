# Chronoscope dev workflow.
#
# Three layers of recipe:
#
#   `just check [target]`  — hermetic ground truth via `nix flake check`
#                            (or a focused subset). Writes .claude/last-check.json
#                            on success; the pre-commit hook keys off this marker.
#
#   `just fmt|test|clippy [target]`
#                          — fast inner loop. Cargo direct in the right dev shell.
#                            Incremental compilation against ./target. Does NOT
#                            satisfy the pre-commit gate — only `just check` does.
#
#   Everything else        — concrete actions (web-dev, fetch-*, corpus-*,
#                            openapi). Each is safe-by-construction so it can
#                            be allowlisted in .claude/settings.json without
#                            opening a permission hole the way `nix develop` would.
#
# Targets, where supported:
#   all              everything (default)
#   nix              .nix files only
#   rust             native cargo workspace (default-members)
#   web              chronoscope-web (wasm32 target)
#   triton           analysis/triton/ python
#   analysis         chronoscope-analysis crate
#   <crate-name>     single crate (core, db, api, api-client, ingestion,
#                                  workers, dev, integrations)

# ---------------------------------------------------------------------------
# Shared bash helpers (substituted into each recipe via `{{ _name }}`).
# ---------------------------------------------------------------------------

_ensure_nix := '''
_ensure_nix() {
    if ! command -v nix &>/dev/null; then
        . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null \
        || . /nix/var/nix/profiles/default/etc/profile.d/nix.sh 2>/dev/null \
        || { echo "error: nix not found" >&2; exit 1; }
    fi
}
'''

# Map a target name to the dev shell that has the right deps. Used by
# fmt/test/clippy to pick the smallest viable shell — `clippy web` re-execs
# into `web`, not `analysis`, so we don't drag in weights+corpus.
_shell_for_target := '''
_shell_for_target() {
    case "$1" in
        all|analysis) echo "analysis" ;;
        web)          echo "web" ;;
        triton)       echo "triton" ;;
        nix)          echo "default" ;;
        rust|api|core|db|api-client|ingestion|workers|dev|integrations)
                      echo "api" ;;
        *) echo "unknown target: $1" >&2
           echo "valid: all, nix, rust, web, triton, analysis, api, core, db, api-client, ingestion, workers, dev, integrations" >&2
           return 1 ;;
    esac
}
'''

# Re-exec the current recipe inside the right dev shell, if not already in one.
_nix_reexec_for_target := '''
_nix_reexec_for_target() {
    local recipe="$1" target="$2"
    if [ -n "${IN_NIX_SHELL:-}" ]; then return 0; fi
    local shell
    shell=$(_shell_for_target "$target") || exit 1
    exec nix develop ".#$shell" --command just "$recipe" "$target"
}
'''

# Build cached analysis-results derivation and pin as GC root.
_build_analysis_results := '''
_build_analysis_results() {
    export ANALYSIS_RESULTS=$(nix build .#analysis-results --no-link --print-out-paths)
    nix-store --realise "$ANALYSIS_RESULTS" --add-root .nix-gc-roots/analysis-results > /dev/null 2>&1 || true
}
'''


# ---------------------------------------------------------------------------
# Ground truth: hermetic checks via `nix flake check` (or focused subset).
# Writes .claude/last-check.json on success.
# ---------------------------------------------------------------------------

# Run hermetic checks for [target] (default: all). On success, writes
# .claude/last-check.json so the pre-commit hook can confirm the gate
# was passed against the current tree state.
check target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    SYS=$(nix eval --impure --expr 'builtins.currentSystem' --raw)
    case "{{ target }}" in
        all)
            nix flake check
            ;;
        nix)
            nix build ".#checks.$SYS.nix-lint" --no-link
            ;;
        rust)
            nix build \
                ".#checks.$SYS.fmt" \
                ".#checks.$SYS.clippy" \
                ".#checks.$SYS.doctest" \
                ".#checks.$SYS.llvm-cov" \
                --no-link
            ;;
        web)
            # web-test (the browser suite) is intentionally absent: it is
            # ordered after the compile/coverage-heavy checks (see nix/rust.nix)
            # so it runs unstarved, which makes it a whole-gate check. Run it via
            # `just check` (full) or iterate with `just test web` in the web shell.
            nix build \
                ".#checks.$SYS.web-build" \
                ".#checks.$SYS.web-test-build" \
                ".#checks.$SYS.web-clippy" \
                --no-link
            ;;
        triton)
            nix build \
                ".#checks.$SYS.triton-fmt" \
                ".#checks.$SYS.triton-lint" \
                ".#checks.$SYS.triton-typecheck" \
                ".#checks.$SYS.triton-test" \
                --no-link
            ;;
        *)
            echo "error: \`just check\` only accepts coarse targets (hermetic)." >&2
            echo "valid: all, nix, rust, web, triton" >&2
            echo "for per-crate iteration, use \`just clippy <crate>\` or \`just test <crate>\`." >&2
            exit 1
            ;;
    esac
    mkdir -p .claude
    HASH=$(.claude/hooks/tree-hash.sh)
    cat > .claude/last-check.json <<EOF
    {
      "hash": "$HASH",
      "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
      "target": "{{ target }}"
    }
    EOF
    echo "✓ check ({{ target }}) passed; marker written"

# ---------------------------------------------------------------------------
# Fast inner loop: cargo direct, dev shell. NOT a substitute for `just check`.
# ---------------------------------------------------------------------------

# Apply formatting (does not check; use `just check` for that).
fmt target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target fmt "{{ target }}"
    case "{{ target }}" in
        all)
            find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 \
              | xargs -0 nixfmt
            cargo fmt
            cargo fmt -p chronoscope-web
            (cd analysis/triton && ruff format .)
            ;;
        nix)
            find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 \
              | xargs -0 nixfmt
            ;;
        rust)
            cargo fmt
            cargo fmt -p chronoscope-web
            ;;
        web)
            cargo fmt -p chronoscope-web
            ;;
        triton)
            cd analysis/triton && ruff format .
            ;;
        *)
            cargo fmt -p chronoscope-{{ target }}
            ;;
    esac

# Run tests (cargo direct, fast incremental). Use `just check` for the
# hermetic version that gates commits.
test target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target test "{{ target }}"
    case "{{ target }}" in
        all)
            cargo test
            cargo test -p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4
            (cd analysis/triton && pytest -v)
            ;;
        rust)
            cargo test
            ;;
        web)
            cargo test -p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4
            ;;
        triton)
            cd analysis/triton && pytest -v
            ;;
        *)
            cargo test -p chronoscope-{{ target }}
            ;;
    esac

# Run clippy (cargo direct, fast incremental). Use `just check` for the
# hermetic version.
clippy target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target clippy "{{ target }}"
    case "{{ target }}" in
        all)
            cargo clippy --all-targets -- -D warnings
            cargo clippy -p chronoscope-web --target wasm32-unknown-unknown -- -D warnings
            (cd analysis/triton && ruff check .)
            ;;
        rust)
            cargo clippy --all-targets -- -D warnings
            ;;
        web)
            cargo clippy -p chronoscope-web --target wasm32-unknown-unknown -- -D warnings
            ;;
        triton)
            cd analysis/triton && ruff check .
            ;;
        *)
            cargo clippy -p chronoscope-{{ target }} --all-targets -- -D warnings
            ;;
    esac

# ---------------------------------------------------------------------------
# Dev servers and concrete actions.
# ---------------------------------------------------------------------------

# Start the integrated web dev server (API + Trunk live reload).
# The binary picks free ports for API + Trunk automatically — no need to
# kill anything else on common ports.
web-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#web --command just web-dev
    fi
    cargo run -p chronoscope-dev --bin web-dev

# Build the OpenAPI spec (Nix) and report its store path — inspect the contract.
openapi:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    out=$(nix build .#openapi --no-link --print-out-paths)
    echo "openapi spec: $out/openapi.json"

# Regenerate the Xcode project. Nix splices the API-package + Swift-tool store
# paths into the xcodegen spec; pinning the spec keeps that closure alive.
xcodegen:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#ios --command just xcodegen
    fi
    mkdir -p .nix-gc-roots
    spec=$(nix build .#ios-project-spec --out-link .nix-gc-roots/ios-project-spec --print-out-paths)
    cd ios
    xcodegen generate --spec "$spec" --project-root . --project .

# ---------------------------------------------------------------------------
# Data fetches. Each is a thin wrapper around `nix build` + GC root pinning.
# ---------------------------------------------------------------------------

# Fetch model weights (DINOv3 + SAM3) from Hugging Face.
# Requires HF_TOKEN env var on first fetch (the FOD will fail with
# instructions if missing).
fetch-weights:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching DINOv3 weights..."
    nix build .#dinov3-weights --impure --no-link
    echo "==> Fetching SAM3 weights..."
    nix build .#sam3-weights --impure --no-link
    echo "Done. Weights will be pinned as GC roots on next analysis/triton shell entry."

# Fetch corpus images from external URLs.
fetch-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching corpus images..."
    nix build .#corpus-images --no-link
    echo "Done. Corpus images will be pinned as GC roots on next analysis shell entry."

# Fetch everything: model weights + corpus images.
# Single nix build so all FODs fetch in parallel (different hosts —
# HF, corpus URLs — so concurrency is a clean win).
fetch-all:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching weights, corpus in parallel..."
    nix build \
        .#dinov3-weights \
        .#sam3-weights \
        .#corpus-images \
        --impure --no-link
    echo "Done."

# Build + pin the Wikidata architectural-entities set. Runs the bulk pipeline:
# fetch the ~109GB dump (FOD, on first run), resolve the P279 type set, and
# filter down to architectural entities. Expensive — run rarely. The
# --out-link is itself the GC root, so ordinary shell entry never triggers it.
fetch-wikidata:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    mkdir -p .nix-gc-roots
    echo "==> Building + pinning architectural-entities set (downloads the dump on first run)..."
    nix build .#wikidata-arch-entities --out-link .nix-gc-roots/wikidata-arch-entities
    echo "Done. Pinned at .nix-gc-roots/wikidata-arch-entities/entities.jsonl"

# Build + pin a SQLite facts DB (size: curated | 1k | 100k | full). Every size
# above `curated` needs the architectural-entities set (and thus the dump).
fetch-wikidata-db size="full":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    mkdir -p .nix-gc-roots
    nix build ".#wikidata-facts-db-{{ size }}" --out-link ".nix-gc-roots/wikidata-facts-db-{{ size }}"
    echo "Done. Pinned at .nix-gc-roots/wikidata-facts-db-{{ size }}/facts.db"

# ---------------------------------------------------------------------------
# Corpus tooling. Live in their own world because the corpus pipeline has
# specific feature flags and a separate analysis-results derivation.
# ---------------------------------------------------------------------------

# Generate / update corpus FOD hashes (fetches new URLs, skips existing).
corpus-hash:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-hash
    fi
    cargo run --features corpus-test -p chronoscope-analysis --bin corpus-fetch -- hash

# Run the corpus test suite (per-image + per-cluster, with known-issue tracking).
corpus-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _build_analysis_results }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-test
    fi
    _build_analysis_results
    cargo test --features corpus-test -p chronoscope-analysis --test corpus_tests

# Run the corpus test suite with VLM (requires remote Triton).
corpus-test-vlm:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _build_analysis_results }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-test-vlm
    fi
    _build_analysis_results
    cargo test --features corpus-test-vlm -p chronoscope-analysis --test corpus_tests
