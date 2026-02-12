# Development Practices

This document expands on the development tenets from [CLAUDE.md](../CLAUDE.md).

## Feedback Mechanisms

The same properties that help LLM agents work effectively also make human development more pleasant. We invest in feedback mechanisms that catch errors early:

### Strong Types

- Rust and Swift both have algebraic data types (enums with associated data)
- We use these to make invalid states unrepresentable, meaning more bugs caught statically
- Type-safe IDs (`UserId`, `ResearchUrlId`) prevent mixing up identifiers
- The OpenAPI spec provides type safety across the API boundary

### Strict Lints

Workspace-wide clippy configuration in `Cargo.toml`:

```toml
[workspace.lints.clippy]
unwrap_used = "deny"    # Handle errors properly
expect_used = "deny"    # No panicking on errors
panic = "deny"          # No intentional panics
doc_markdown = "deny"   # Consistent documentation
```

Additional lint configuration in `clippy.toml` bans `sleep` functions to prevent accidental blocking.

Swift uses SwiftFormat and SwiftLint, enforced via Xcode build phases.

### Comprehensive Tests

- API tests use real databases (in-memory SQLite) and simulated passkeys via `webauthn-authenticator-rs`
- Worker tests use VCR-style HTTP fixtures for reproducible network behavior
- iOS UI tests exercise the full app with mock API clients
- Tests verify behavior, not implementation details
- Tests are real code, and we strive to keep them well factored and readable with similar standards to other code

### Fast Local Iteration

One terminal command and one Xcode shortcut for a fully functioning system:

```bash
cargo run -p chronoscope-dev  # Starts ngrok + API + workers
# Then Cmd+R in Xcode
```

The dev server automatically updates `ios/Local.xcconfig` with the ngrok URL, so Xcode rebuilds pick up the current tunnel automatically. The dependency between the two is necessary because iOS's passkey implementation needs the API domain to be registered in the app's signed entitlements.

## Predictable Performance

### Query Verification

All SQL queries are:

1. Defined in a central `queries.rs` module with named constants
2. Verified at startup with `EXPLAIN QUERY PLAN`
3. Required to use indexes - no full table scans allowed

This catches many performance regressions before they reach production.

### Keyset Pagination

Lists use cursor-based (keyset) pagination instead of offset pagination:

- Consistent performance regardless of page depth
- No skipped or duplicated items when data changes
- Cursors are opaque tokens encoding the last seen item

## OpenAPI-First Development

The API contract is the source of truth:

1. Rust endpoints are defined with Dropshot macros
2. Request/response types derive `JsonSchema` via schemars
3. `cargo run --bin openapi -- api/target/openapi.json` generates the spec
4. iOS project symlinks to this file at `ios/ChronoscopeAPI/Sources/ChronoscopeAPI/openapi.json`
5. Swift OpenAPI Generator creates type-safe client code at Xcode build time

To add a new endpoint:

1. Define the Rust handler with Dropshot macros
2. Add request/response types with schemars derives
3. Register the endpoint in the API
4. Run `cargo run --bin openapi` (or let Xcode pre-build do it)
5. Use the generated Swift types immediately

## Testing Philosophy

### Tests for Correctness

Every test should answer: "What bug would this catch?"

- Don't write tests just to increase coverage numbers
- Focus on edge cases, error conditions, and invariants
- Test behavior, not implementation details
- If refactoring breaks a test but not the behavior, the test was wrong

### Test Organization

**Rust:**
- `api/src/tests/` - API endpoint tests organized by feature
- Workers use `record-fixtures` feature for HTTP fixture recording
- In-memory SQLite for fast, isolated database tests

**Swift:**
- `MockAPIClient` provides predictable responses for UI tests and previews
- UI tests use launch arguments (`--uitesting`, `--authenticated`) to configure test scenarios
- Protocol-based `APIProtocol` enables dependency injection

## Code Organization

### Crate Structure

| Crate | Purpose |
|-------|---------|
| `analysis` | Image analysis pipeline (Triton gRPC client, SAM3/VLM) |
| `api` | REST API server, authentication, endpoints |
| `db` | Database layer, models, queries |
| `dev` | Development server with ngrok integration |
| `integrations` | Domain-specific integrations (Reddit, Instagram), HTTP client abstraction |
| `workers` | Background processing, URL fetching, content extraction |
