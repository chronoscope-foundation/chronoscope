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
| `api`      | default + sqlite/openssl/spatialite/protobuf + WIKIDATA         | Backend / API server work                    |
| `web`      | api + wasm toolchain + trunk + tailwind + chromium + WEB_DIST    | Frontend; running `web-dev`; browser tests   |
| `analysis` | api + Python analysis env + weights + corpus                     | Iterating on `chronoscope-analysis` correctness |
| `triton`   | Python analysis env + weights + rust toolchain (for schematool)  | Triton harness / serving config              |
| `ios`      | xcodegen + swiftformat/swiftlint/xcbeautify                      | iOS project generation & Swift lint/format   |

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
just fetch-all                             # weights + corpus
just fetch-wikidata                        # build+pin the architectural-entities set (bulk ingestion source)
```

Fetched data is pinned as GC roots under `.nix-gc-roots/` (gitignored).

## Commit gate: `just check`

The **full** `just check` (no target) is the hermetic ground-truth gate
for commits — the complete `nix flake check`. It writes
`.claude/last-check.json` on success, a marker recording the working-tree
state at the moment of the check.

**Run the full `just check` before committing.** `just check <target>`
(rust/web/triton/nix) runs a faster scoped subset for iteration, but the
subsets do not add up to the whole gate: the browser suite (`web-test`) is
ordered after the heavy checks so its headless-Chrome event loop isn't
starved, which means it runs **only** in the full `just check`. A scoped
`just check web` builds the WASM but never launches the browser tests.

A pre-commit hook at `.claude/hooks/precommit-check.sh` (registered in
`.claude/settings.json`) checks the marker against the current tree on
every `git commit`. It **prompts for confirmation** unless the full
`just check` (target `all`) passed against the current tree — that is, when
the marker is missing, the tree has drifted, or the marker is from a scoped
subset. Confirming still lets a deliberate commit (e.g. a WIP checkpoint)
through; the prompt just keeps skipping the full gate a conscious choice
rather than an accident.

`just test`, `just clippy`, `just fmt` are the **fast inner loop**:
cargo direct, dev shell, incremental compilation. They are deliberately
**not** a substitute for `just check` — they don't write the marker, and
the pre-commit hook only honors the marker.

## Recipe inventory

```bash
# Hermetic gate (writes .claude/last-check.json on success)
just check                  # everything incl. browser tests — the commit gate
just check rust             # workspace fmt + clippy + test + coverage
just check web              # WASM build + browser-test build + wasm clippy
                            #   (browser tests run ONLY in the full `just check`)
just check triton           # Python ruff + mypy + pytest
just check nix              # Nix lint (nixfmt + statix + deadnix)
# Targeted subsets are for iteration; only the full `just check` runs the
# whole gate (web-test is ordered after the heavy checks, so it lives there).

# Fast inner loop (cargo direct; do not satisfy the commit gate)
just fmt   [target]         # apply formatting
just test  [target]         # cargo test
just clippy [target]        # cargo clippy

# Per-crate target (test/clippy): core, db, api, api-client, ingestion,
# workers, dev, integrations, web, triton, analysis
#   e.g. just clippy core   → cargo clippy -p chronoscope-core -- -D warnings

# Concrete actions
just web-dev                # integrated dev server (API + Trunk live reload; auto-picks free ports)
just openapi                # build the raw OpenAPI spec (inspect the contract)
just xcodegen               # splice store paths into the xcodegen spec, regenerate the Xcode project
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

## Wire stability conventions

The fact-store grammar's serialized form is content-addressed via JCS +
SHA-256: a `CommitId` is the hash of the producer-form `submit::Commit` —
author, `recorded_at`, the declaration lists, and the facts. Any change to
the serialize shape of a type reachable from `submit::Commit` changes
every commit's `CommitId`.

This is greenfield — no deployed store, no other readers, no
backward-compatibility constraint. Change wire shapes when the right shape
calls for it and regenerate the goldens in the same commit. The golden
tests in `core/src/facts/wire_goldens.rs` catch *unintentional* shape
changes — a failing golden is the "did you mean to change this?" signal.
Each golden byte-pins the JCS form and round-trips the value through
`serde_json` (the streaming serializer catches tagged-enum shapes JCS
tolerates).

Declare every grammar sum/product with `#[grammar_type]` (from the
`chronoscope-macros` crate). It is the one source of the grammar's serde
conventions: a single `"type"` internal tag, `snake_case` variant names,
`deny_unknown_fields`, and derived `Serialize`/`Deserialize`/`JsonSchema`.
Variants and structs must use named fields — a tuple/newtype variant is a
compile error, since internal tagging flattens an unnamed payload beside
the tag and collides. Add the comparison/`Debug`/`Clone` derives yourself
in a `#[derive(..)]` alongside (float-bearing types carry hand-written
`Eq`/`Hash`/`Ord`). Exemptions: `Location`/`UnresolvedLocation` keep
hand-written `Deserialize`; the transparent leaf newtypes (ids, validated
strings) use the `*_newtype!` macros.

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