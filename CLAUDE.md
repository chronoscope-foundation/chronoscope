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

All tooling comes from Nix. Install [Nix](https://nixos.org/download/)
(Determinate Nix recommended), then either:

- **direnv**: `direnv allow` (auto-activates on `cd`)
- **Manual**: `nix develop`

`just` recipes auto-wrap with the right `nix develop` when you're not
already in a shell — pick the closest recipe and let it handle the
shell selection.

### Dev shells by component

The flake exposes one shell per project area, sized to what that area
needs. Shells are derivations like everything else; no shells "depend on"
each other beyond what they explicitly compose.

| Shell      | What's in it                                                     | When to use                                  |
|------------|------------------------------------------------------------------|----------------------------------------------|
| `default`  | rust toolchain + just + nix lint tools                           | Poking at the project, running `just <recipe>` |
| `api`      | default + sqlite/openssl/spatialite/protobuf + WIKIDATA + REGIONS | Backend / API server work                    |
| `web`      | api + wasm toolchain + trunk + tailwind + chromium + WEB_DIST    | Frontend; running `web-dev`; browser tests   |
| `analysis` | api + Python analysis env + weights + corpus                     | Iterating on `chronoscope-analysis` correctness |
| `triton`   | Python analysis env + weights + rust toolchain (for schematool)  | Triton harness / serving config              |

`just` recipes pick the smallest shell that covers their target
(e.g. `just clippy web` enters `web`, `just check triton` runs hermetically
via Nix). The `default` shell is intentionally minimal; cargo invocations
beyond toolchain queries will fail to link there.

### Component-specific notes

Sub-CLAUDE.md files at `web/`, `api/`, `analysis/`, `analysis/triton/`,
and `nix/` carry area-specific knowledge that auto-loads when Claude
touches files in those subtrees. Read them when you start working in a
new area; they cover the non-obvious bits (wasm32 target gotcha,
OpenAPI client regen, corpus FOD layout, HF cache layout, etc.).

### Fetching data

```bash
HF_TOKEN=hf_... just fetch-weights        # ~2GB model weights (gated repos; needs HF token)
just fetch-corpus                          # corpus images from external URLs
just fetch-regions [italy|world]           # OSM regions DB (italy default ~2GB; world ~70GB)
just fetch-all                             # weights + corpus + italy regions
```

Fetched data is pinned as GC roots under `.nix-gc-roots/` (gitignored).

## Commit gate: `just check`

`just check` is the hermetic ground-truth gate for commits. It is
equivalent to `nix flake check` (or a focused subset for `just check
<target>`) and writes `.claude/last-check.json` on success — a marker
that records the working-tree state at the moment of the check.

A pre-commit hook at `.claude/hooks/precommit-check.sh` (registered in
`.claude/settings.json`) checks the marker against the current tree on
every `git commit`. If the tree has changed since the last successful
`just check`, the hook injects an advisory reminder for Claude to
re-run. The hook is non-blocking — you can still commit through it
deliberately, but the reminder is there.

`just test`, `just clippy`, `just fmt` are the **fast inner loop**:
cargo direct, dev shell, incremental compilation. They are deliberately
**not** a substitute for `just check` — they don't write the marker, and
the pre-commit hook only honors the marker.

## Recipe inventory

```bash
# Hermetic gate (writes .claude/last-check.json on success)
just check                  # everything (nix flake check)
just check rust             # workspace fmt + clippy + test + coverage
just check web              # WASM build + browser-test build + wasm clippy
just check triton           # Python ruff + mypy + pytest
just check nix              # Nix lint (nixfmt + statix + deadnix)

# Fast inner loop (cargo direct; do not satisfy the commit gate)
just fmt   [target]         # apply formatting
just test  [target]         # cargo test
just clippy [target]        # cargo clippy

# Per-crate target (test/clippy): core, db, api, api-client, ingestion,
# workers, dev, integrations, web, triton, analysis
#   e.g. just clippy core   → cargo clippy -p chronoscope-core -- -D warnings

# Concrete actions
just web-dev                # integrated dev server (API + Trunk live reload; auto-picks free ports)
just openapi                # regenerate api/target/openapi.json
just corpus-hash            # add hashes for new corpus URLs
just corpus-test            # run Rust corpus test suite
just corpus-test-vlm        # corpus tests + VLM (needs remote Triton)
```

### Workflow examples

```bash
# I'm editing the web crate; verify it still compiles cleanly
just clippy web

# I'm about to commit a backend change; run the hermetic gate
just check

# I touched an API endpoint; need to regen and re-verify
just openapi
just check rust

# I'm running the integrated dev server
just web-dev      # picks free ports automatically; no port collision
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
