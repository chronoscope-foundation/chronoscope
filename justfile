# Ensure nix is on PATH, sourcing the daemon profile as a fallback.
_ensure_nix := '''
_ensure_nix() {
    if ! command -v nix &>/dev/null; then
        . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null \
        || . /nix/var/nix/profiles/default/etc/profile.d/nix.sh 2>/dev/null \
        || { echo "error: nix not found" >&2; exit 1; }
    fi
}
'''

# Helper: ensure we're in a Nix dev shell, re-exec if not.
# Placed at the top of each recipe's shebang script:
#   if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec <recipe>; fi
#
# Accepts an optional shell name as second argument (default: "default"):
#   _nix_reexec <recipe> [shell]
_nix_reexec := '''
_nix_reexec() {
    local recipe="$1"
    local shell="${2:-default}"
    ''' + _ensure_nix + '''
    _ensure_nix
    exec nix develop ".#$shell" --command just "$recipe"
}
'''

# Fetch model weights (DINOv3 + SAM3) from Hugging Face.
# Requires HF_TOKEN env var (the derivation will fail with instructions if missing).
fetch-weights:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching DINOv3 weights..."
    nix build .#dinov3-weights --impure --no-link
    echo "==> Fetching SAM3 weights..."
    nix build .#sam3-weights --impure --no-link
    echo "Done. Weights will be pinned as GC roots on next shell entry."

# Fetch corpus images from external URLs.
fetch-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching corpus images..."
    nix build .#corpus-images --no-link
    echo "Done. Corpus images will be pinned as GC roots on next shell entry."

# Build administrative regions database (Italy extract).
# Downloads ~2 GB PBF, filters boundaries, runs cosmogony, builds SpatiaLite DB.
# Result is pinned as a GC root so it survives garbage collection.
fetch-regions:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    echo "==> Building Italy regions database..."
    REGIONS_DB=$(nix build .#regions-italy-db --no-link --print-out-paths)
    _gc_root_dir="$(git rev-parse --show-toplevel 2>/dev/null || echo .)/.nix-gc-roots"
    mkdir -p "$_gc_root_dir"
    nix-store --realise "$REGIONS_DB" --add-root "$_gc_root_dir/regions-db" > /dev/null 2>&1
    echo "Done. Regions DB at: $REGIONS_DB/regions.sqlite"

# Fetch everything: model weights + corpus images.
fetch-all: fetch-weights fetch-corpus

# Run all checks: Nix, Rust, Python
check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec check analysis; fi

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

# Run browser tests against headless Chrome (requires Chrome/Chromium)
# Test frontend dist is provided via $WEB_DIST from the nix shell.
web-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec web-test; fi
    echo "==> Running browser tests"
    cargo test -p chronoscope-dev --test web

# Start web dev server (API + Trunk live reload)
web-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec web-dev; fi
    cargo run -p chronoscope-dev --bin web-dev

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
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec corpus-test corpus; fi
    {{ _build_analysis_results }}
    _build_analysis_results
    cargo test --features corpus-test -p chronoscope-analysis --test corpus_tests

# Run corpus test suite with VLM (requires remote Triton)
corpus-test-vlm:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _nix_reexec }}
    if [ -z "${IN_NIX_SHELL:-}" ]; then _nix_reexec corpus-test-vlm corpus; fi
    {{ _build_analysis_results }}
    _build_analysis_results
    cargo test --features corpus-test-vlm -p chronoscope-analysis --test corpus_tests
