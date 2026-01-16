# Run all checks: format, lint, test, coverage
[no-cd]
check:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test
    cargo llvm-cov

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
