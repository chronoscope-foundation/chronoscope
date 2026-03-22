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

## Development Environment

All tooling comes from Nix. Install [Nix](https://nixos.org/download/) (Determinate Nix recommended), then either:

- **direnv**: `direnv allow` (auto-activates on `cd`)
- **Manual**: `nix develop`

`just` commands auto-wrap with `nix develop` if you're not already in the shell.

### Shell Tiers

Three dev shells provide increasing levels of data, so the base shell starts fast without large downloads:

| Shell | What it adds | Use case |
|-------|-------------|----------|
| `default` | Rust + Python + lint tools | Web dev, API work, most of the repo |
| `analysis` | + model weights (DINOv3, SAM3) | Analysis pipeline tests |
| `corpus` | + corpus images | Full corpus test suite |

```bash
nix develop              # default — no large downloads
nix develop .#analysis   # requires: just fetch-weights
nix develop .#corpus     # requires: just fetch-weights + just fetch-corpus
```

`just` recipes automatically select the right shell tier (e.g., `just check` uses `analysis`, `just corpus-test` uses `corpus`).

### Fetching Data

```bash
HF_TOKEN=hf_... just fetch-weights   # ~2GB model weights (one-time, needs HF token)
just fetch-corpus                     # corpus images from external URLs
just fetch-all                        # both of the above
```

Fetched data is pinned as GC roots in `.nix-gc-roots/` so Nix garbage collection won't sweep it.

## Commit Requirements

Every commit must pass `just check`, which runs Nix linting (nixfmt, statix, deadnix), Rust checks (fmt, clippy, test, coverage), and Python checks (ruff, mypy, pytest). Line coverage must stay above 75%.

## Quick Reference

```bash
# Run all checks (Nix + Rust + Python — needs analysis shell)
just check

# Auto-fix all formatting
just fmt

# Start web frontend dev server (Trunk live reload)
just web-dev

# Start dev server (ngrok + API + workers)
cargo run -p chronoscope-dev

# Generate OpenAPI spec
cargo run --bin openapi -- api/target/openapi.json

# Hermetic sandboxed checks (CI-style, no GPU required)
nix flake check

# Run individual Nix checks
nix build .#checks.$(nix eval --impure --expr builtins.currentSystem --raw).triton-test

# Corpus tests (needs corpus shell — builds GPU analysis results on demand)
just corpus-test
```

## Cross-Language Testing

Python tests call `schematool` (a Rust binary from `analysis/src/bin/schematool.rs`) to validate that Python model output matches Rust schema expectations. This catches schema drift between the two languages.

- **`nix flake check`**: schematool comes from `rust.packages.default` (the workspace build)
- **`just check`**: schematool is built by `cargo build --bin schematool` and added to PATH
- **Model weights**: SAM3 and DINOv3 weights are fetched via `just fetch-weights` into the Nix store as fixed-output derivations (`hf download` in a sandboxed FOD). `HF_HUB_OFFLINE=1` is set in the analysis/corpus shells — missing weights fail hard, never download silently.

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
