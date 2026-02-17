# Run all checks: format, lint, test, coverage
[no-cd]
check:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test
    cargo llvm-cov --fail-under-lines 75

# Format code
[no-cd]
fmt:
    cargo fmt

# Run clippy
[no-cd]
clippy:
    cargo clippy -- -D warnings

# Run tests
[no-cd]
test:
    cargo test

# Run tests with coverage (75% minimum)
[no-cd]
coverage:
    cargo llvm-cov --fail-under-lines 75

# Download corpus images (no models needed)
corpus-download:
    cargo run --features corpus-test -p chronoscope-analysis --bin corpus -- download

# Run corpus test suite (per-image + per-cluster, with known-issue tracking)
corpus-test: triton-venv
    cargo test --features corpus-test -p chronoscope-analysis --test corpus_tests

# Run corpus test suite with VLM (requires remote Triton)
corpus-test-vlm: triton-venv
    cargo test --features corpus-test-vlm -p chronoscope-analysis --test corpus_tests

# Set up Triton test venv
triton-venv:
    #!/usr/bin/env bash
    set -euo pipefail
    cd analysis/triton
    if [ ! -d .venv ]; then
        python3 -m venv .venv
    fi
    .venv/bin/pip install -q -r requirements.txt

# Format Triton Python code
triton-fmt: triton-venv
    cd analysis/triton && .venv/bin/ruff format .

# Check Triton Python formatting
triton-fmt-check: triton-venv
    cd analysis/triton && .venv/bin/ruff format --check .

# Lint Triton Python code
triton-lint: triton-venv
    cd analysis/triton && .venv/bin/ruff check .

# Fix Triton Python lint issues
triton-lint-fix: triton-venv
    cd analysis/triton && .venv/bin/ruff check --fix .

# Type check Triton Python code
# Run mypy on each model file separately to avoid duplicate module name conflicts
triton-typecheck: triton-venv
    #!/usr/bin/env bash
    set -euo pipefail
    cd analysis/triton
    # Check test/mock files together
    .venv/bin/mypy --ignore-missing-imports mock_triton.py conftest.py test_models.py
    # Check each model file separately (Triton requires model.py naming)
    for f in models/*/1/model.py models/*/1/baml_converter.py; do
        if [ -f "$f" ]; then
            .venv/bin/mypy --ignore-missing-imports "$f"
        fi
    done

# Run Triton model tests
triton-test: triton-venv
    cd analysis/triton && .venv/bin/pytest -v

# Run Triton tests with property-based testing (more examples)
triton-test-full: triton-venv
    cd analysis/triton && .venv/bin/pytest -v --hypothesis-seed=0

# Run all Triton Python checks (format, lint, typecheck, test)
triton-check: triton-venv
    #!/usr/bin/env bash
    set -euo pipefail
    cd analysis/triton
    echo "==> Checking format..."
    .venv/bin/ruff format --check .
    echo "==> Linting..."
    .venv/bin/ruff check .
    echo "==> Type checking..."
    .venv/bin/mypy --ignore-missing-imports mock_triton.py conftest.py test_models.py
    for f in models/*/1/model.py models/*/1/baml_converter.py; do
        if [ -f "$f" ]; then
            .venv/bin/mypy --ignore-missing-imports "$f"
        fi
    done
    echo "==> Running tests..."
    .venv/bin/pytest -v
