# Chronoscope

## Development Tenets

**Feedback mechanisms everywhere.** Strong types, strict lints, comprehensive tests, and fast local iteration catch errors early. These feedback loops help both LLM agents and human developers - it's not a tradeoff, it's just good engineering.

**Predictable performance.** All SQL queries are factored out and verified with `EXPLAIN QUERY PLAN` at startup. No full table scans allowed. APIs should have predictable, verifiable performance characteristics.

**OpenAPI-first.** The API contract is the source of truth. iOS, web, and CLI clients are all generated from the same OpenAPI spec. Change the Rust endpoint, and the Swift client updates automatically.

**Thin slice, then broaden.** Prove out the full stack end-to-end before building breadth. The current implementation is a thin slice (URLs → workers → iOS app) that establishes patterns for the larger system.

**Tests for correctness, not coverage.** Every test should catch meaningful bugs. We don't write tests to hit coverage numbers - we think critically about what each test validates.

See [docs/development.md](docs/development.md) for detailed practices.

## Project Tenets

See [docs/design.md](docs/design.md) for the full design philosophy. Key points:

- **Uncertainty is data** - vague dates and locations are first-class, not forced into false precision
- **Citations are pervasive** - every assertion requires attribution, machine-checkable where possible
- **Collaborative research** - AI assists humans, doesn't replace them; behavior must be interpretable
- **API-first platform** - all clients are API consumers; easy ingestion for external datasets

## Commit Requirements

Every commit must pass `just check`, which runs formatting, linting, tests, and coverage. Line coverage must stay above 75%.

Run from the repo root for workspace-wide checks, or from a crate directory (e.g., `api/`) to check just that crate.

## Quick Reference

```bash
# Run all checks (fmt, clippy, test, coverage)
just check

# Start dev server (ngrok + API + workers)
cargo run -p chronoscope-dev

# Generate OpenAPI spec
cargo run --bin openapi -- api/target/openapi.json
```

## Code Standards

### Rust

- `#![deny(clippy::unwrap_used)]` - handle errors properly, no unwraps
- `#![deny(unsafe_code)]` - no unsafe code
- OpenAPI-first: Dropshot macros define endpoints, schemars for schema
- Tests use real database (in-memory SQLite) and simulated passkeys

### Swift

- SwiftFormat and SwiftLint enforced via Xcode build phases
- `APIProtocol` abstraction for swapping real/mock implementations
- Mock clients for previews and UI tests

## Architecture Overview

See [docs/architecture.md](docs/architecture.md) for details.

```
┌─────────────┐     OpenAPI      ┌─────────────┐
│   iOS App   │ ←───(generated)──│  Rust API   │
│  (SwiftUI)  │                  │ (Dropshot)  │
└─────────────┘                  └─────────────┘
       │                                │
       │ WebAuthn                       │ SQLite
       ▼                                ▼
   [Passkeys]                      [SQLite]
                                        │
                                        ▼
                                   [Workers]
                                   (URL fetch,
                                    content
                                    extraction,
                                    image analysis)
```
