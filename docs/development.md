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
- iOS UI tests exercise the full app with mock API clients, though nothing runs them while the app is unmaintained
- Tests verify behavior, not implementation details
- Tests are real code, and we strive to keep them well factored and readable with similar standards to other code

### Fast Local Iteration

One command brings up the whole web stack:

```bash
just web-dev   # facts DB + API + Trunk, all on free ports
```

The iOS variant is `cargo run -p chronoscope-dev`, which starts ngrok alongside the API and writes the tunnel URL into `ios/Local.xcconfig`, so Xcode rebuilds pick it up. The tunnel is necessary because iOS's passkey implementation needs the API domain registered in the app's signed entitlements. The app itself is not currently maintained; see the README.

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
3. The `openapi` binary serializes the spec (`packages.openapi`), assembled into the ChronoscopeAPI SwiftPM package (`packages.ios-api-package`)
4. `just xcodegen` splices that package's store path and the Swift tool paths into the xcodegen spec (`packages.ios-project-spec`, via `replaceVars`), pins it so the closure survives GC, and generates `ios/Chronoscope.xcodeproj` in place
5. Swift OpenAPI Generator produces type-safe client code from the package at Xcode build time

The store paths are baked into the generated project, and the spec is content-addressed in the store. `just openapi` builds the raw contract (`packages.openapi`) for inspection. To change the client, edit the hand-written source under `ios/ChronoscopeAPI/` (`Package.swift`, `ChronoscopeAPI.swift`, `openapi-generator-config.yaml`) or the Rust API, then `just xcodegen` to rebuild the spec and regenerate the project.

To add a new endpoint:

1. Define the Rust handler with Dropshot macros
2. Add request/response types with schemars derives
3. Register the endpoint in the API
4. Run `just xcodegen` to rebuild the spec and regenerate the project
5. Use the generated Swift types immediately

## Testing Philosophy

### Tests for Correctness

Every test should answer: "What bug would this catch?"

- Don't write tests just to increase coverage numbers
- Focus on edge cases, error conditions, and invariants
- Test behavior, not implementation details
- If refactoring breaks a test but not the behavior, the test was wrong

### Model Weights

Weights (SAM3, DINOv3) are fetched by `just fetch-weights` into the Nix store
as fixed-output derivations — `hf download` inside a sandboxed FOD, so the one
impure step in the chain is isolated to it. Gated repos need `HF_TOKEN` on the
first fetch; after that the output hash pins the result and builds are pure.

Nothing consumes them at test time. They are inputs to the ONNX exports in
`nix/vision.nix`, which run offline against the fetched store paths and are
deliberately outside the commit gate — see the root `CLAUDE.md`.

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
| `analysis` | Image analysis: SAM 3, DINOv3, Qwen 3.6 through ONNX Runtime and mistral.rs |
| `api` | REST API server, authentication, endpoints |
| `api-client` | Typed HTTP client and the shared contract types the web frontend uses |
| `core` | The fact store: grammar, solvers, projections |
| `db` | Database layer: app schema, fact-store backends, queries |
| `dev` | Development servers (`web-dev`, and the ngrok flow for iOS) |
| `ingestion` | Bulk ingestion from external knowledge bases (Wikidata) |
| `integrations` | Domain-specific integrations (Reddit, Instagram), HTTP client abstraction |
| `macros` | Procedural macros for the fact-store grammar |
| `tools/quantize` | Pre-quantizes Qwen to a UQFF the analysis crate loads |
| `web` | WASM frontend (Leptos) |
| `workers` | Background processing, URL fetching, content extraction |
