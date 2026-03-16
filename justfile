# Helper: ensure we're in a Nix dev shell, re-exec if not.
# Placed at the top of each recipe's shebang script:
#   if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec <recipe>; fi
_nix_reexec := '''
_nix_reexec() {
    # Source Nix profile if nix isn't on PATH
    if ! command -v nix &>/dev/null; then
        . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null \
        || . /nix/var/nix/profiles/default/etc/profile.d/nix.sh 2>/dev/null \
        || { echo "error: nix not found" >&2; exit 1; }
    fi
    # Model weights are gated HF repos that require a one-time --impure build.
    # The shellHook creates GC root symlinks on first `nix develop`. If the
    # directory exists but symlinks are missing/broken, weights were GC'd.
    if [ -d .nix-gc-roots ] && { [ ! -L .nix-gc-roots/dinov3-weights ] || [ ! -L .nix-gc-roots/sam3-weights ]; }; then
        echo "error: Model weights not in Nix store (likely garbage collected)." >&2
        echo "" >&2
        echo "  Run once (requires Hugging Face token):" >&2
        echo "    HF_TOKEN=<your-token> nix build .#dinov3-weights .#sam3-weights --impure" >&2
        echo "" >&2
        echo "  Then re-run your command. The dev shell will pin them as GC roots." >&2
        exit 1
    fi
    exec nix develop --command just "$@"
}
'''

# Run all checks: Nix, Rust, Python
check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec check; fi

    echo "==> Nix"
    find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 | xargs -0 nixfmt --check
    statix check .
    find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 | xargs -0 deadnix --fail -L

    echo "==> Rust"
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test
    cargo llvm-cov --fail-under-lines 75

    # Web crate targets wasm32-unknown-unknown and is excluded from default-members
    # because cargo can't mix native and WASM targets in one invocation.
    echo "==> Web (WASM)"
    cargo fmt -p chronoscope-web --check
    cargo clippy -p chronoscope-web --target wasm32-unknown-unknown -- -D warnings

    echo "==> Python (triton)"
    cargo build --bin schematool
    export PATH="$PWD/target/debug:$PATH"
    cd analysis/triton
    ruff format --check .
    ruff check .
    mypy --ignore-missing-imports mock_triton.py conftest.py test_models.py
    for f in models/*/1/model.py models/*/1/baml_converter.py; do
        if [ -f "$f" ]; then
            mypy --ignore-missing-imports "$f"
        fi
    done
    pytest -v

# Start web frontend dev server (Trunk live reload)
web-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec web-dev; fi
    cd web && trunk serve

# Auto-fix formatting (Nix + Rust + Python)
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec fmt; fi
    find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 | xargs -0 nixfmt
    cargo fmt
    cargo fmt -p chronoscope-web  # excluded from default-members (WASM target)
    cd analysis/triton && ruff format .

# Generate/update corpus FOD hashes (fetches new URLs, skips existing)
corpus-hash:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec corpus-hash; fi
    cargo run --features corpus-test -p chronoscope-analysis --bin corpus-fetch -- hash

# Build analysis results on demand (GPU — cached after first run) and pin as GC root.
# TODO: This calls `nix build` from within `nix develop`, triggering a second flake
# evaluation. Fine today (dev shells aren't sandboxed), but worth revisiting if this
# ever needs to run inside a sandboxed derivation.
_build_analysis_results := '''
_build_analysis_results() {
    export ANALYSIS_RESULTS=$(nix build .#analysis-results --no-link --print-out-paths)
    nix-store --realise "$ANALYSIS_RESULTS" --add-root .nix-gc-roots/analysis-results > /dev/null 2>&1 || true
}
'''

# Run corpus test suite (per-image + per-cluster, with known-issue tracking)
corpus-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec corpus-test; fi
    {{ _build_analysis_results }}
    _build_analysis_results
    cargo test --features corpus-test -p chronoscope-analysis --test corpus_tests

# Run corpus test suite with VLM (requires remote Triton)
corpus-test-vlm:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec corpus-test-vlm; fi
    {{ _build_analysis_results }}
    _build_analysis_results
    cargo test --features corpus-test-vlm -p chronoscope-analysis --test corpus_tests
