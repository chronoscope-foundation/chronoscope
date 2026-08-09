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
| `analysis` | api + onnxruntime + corpus images                                | Iterating on `chronoscope-analysis` correctness |
| `ios`      | xcodegen + swiftformat/swiftlint/xcbeautify                      | iOS project generation & Swift lint/format   |
| `deploy`   | gcloud + skopeo (nix: transport)                                 | Pushing the API image, rolling Cloud Run     |
| `infra`    | opentofu (google + cloudflare providers from nixpkgs) + gcloud   | Declaring cloud resources; publishing the frontend |

`just` recipes pick the smallest shell that covers their target
(e.g. `just clippy web` enters `web`, `just check web` runs hermetically
via Nix). The `default` shell is intentionally minimal; cargo invocations
beyond toolchain queries will fail to link there.

### Component-specific notes

Sub-CLAUDE.md files at `web/`, `api/`, and `nix/` carry area-specific
knowledge that auto-loads when Claude touches files in those subtrees.
Read them when you start working in a new area; they cover the
non-obvious bits (wasm32 target gotcha, OpenAPI client regen, corpus FOD
layout, GC root pinning, etc.).

### Fetching data

```bash
HF_TOKEN=hf_... just fetch-weights        # ~2GB model weights (gated repos; needs HF token)
just fetch-models [dinov3_resolution]      # ONNX exports the vision crate loads (224 or 448)
just fetch-corpus                          # corpus images from external URLs
just fetch-all                             # weights + corpus
just fetch-wikidata                        # build+pin the architectural-entities set (bulk ingestion source)
just fetch-wikidata-db [size]              # build+pin a SQLite facts DB (curated|1k|100k|full)
```

Fetched data is pinned as GC roots under `.nix-gc-roots/` (gitignored).

**Never reference a `.nix-gc-roots/` path in code** — not in Rust, not in
`justfile` recipes, nowhere. It exists *only* to keep Nix's GC from reclaiming a
realized derivation; it is a keep-alive symlink, not a dependency handle. Reading
from it breaks Nix's ability to analyze what needs realizing, which is exactly
what forces "not fetched, run `just fetch-…`" errors instead of Nix just building
the thing. Always realize via the Nix expression and use the store path Nix
reports:

```bash
path="$(nix build .#attr --out-link .nix-gc-roots/attr --print-out-paths)"
```

`--print-out-paths` gives the `/nix/store/…` path your code uses; `--out-link`
pins the GC root as a side effect. The *only* place `.nix-gc-roots/` may appear is
as the argument to `--out-link` / `--add-root` (the code that produces the root).
See the `web-dev` / `openapi` / `xcodegen` recipes for the pattern.

## Commit gate: `just check`

The **full** `just check` (no target) is the hermetic ground-truth gate
for commits — the complete `nix flake check`. It writes
`.claude/last-check.json` on success, a marker recording the working-tree
state at the moment of the check.

**Run the full `just check` before committing.** `just check <target>`
(rust/web/nix) runs a faster scoped subset for iteration, but the
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

### Linux checks: `just check linux`

`nix flake check` evaluates only the current system, so on a darwin machine
the Linux outputs never build. `just check linux` asks for them by name
(`checks.x86_64-linux.oci-api-boots` plus the workspace suite under
`llvm-cov`) and needs a builder for that system.

Reach for it when a change could land differently on Linux:

- linking or dynamic libraries (RPATH, `dlopen`, the SpatiaLite load path)
- the container image or the environment it runs under
- process signals and shutdown
- filesystem assumptions (`/tmp`, `/etc/hosts`, anything a sandbox may omit)

It is deliberately **outside** `just check`, so passing the commit gate says
nothing about it: a cross-platform builder is not something every contributor
has, and CI will carry this later.

### Model tests: `just model-test`

Outside the gate for the same reason: the tests that load a real model need a
multi-hundred-MB ONNX export hanging off HF-token weight FODs, which a pure
`nix flake check` cannot realize. The recipe builds the reference fixtures,
pins them, and runs the suite; each fixture holds its export in its own
closure, so realizing one realizes the graph it describes.

Those tests are `#[ignore]`d rather than feature-gated, so they compile in
every build and their count stays visible in ordinary test output. The
deterministic half of the same comparison — the torchvision resize golden — is
pure and stays in `just check`.

`just test`, `just clippy`, `just fmt` are the **fast inner loop**:
cargo direct, dev shell, incremental compilation. They are deliberately
**not** a substitute for `just check` — they don't write the marker, and
the pre-commit hook only honors the marker.

## Recipe inventory

```bash
# Hermetic gate (writes .claude/last-check.json on success)
just check                  # everything incl. browser tests — the commit gate
just check rust             # workspace fmt + clippy + rustdoc + test + coverage
                            #   + both Postgres suites (db backend, api server)
just check web              # WASM build + browser-test build + wasm clippy
                            #   (browser tests run ONLY in the full `just check`)
just check nix              # Nix lint (nixfmt + statix + deadnix)
just check linux            # x86_64-linux: container boot + workspace suite
                            #   (needs a Linux builder; not part of the gate)
# Targeted subsets are for iteration; only the full `just check` runs the
# whole gate (web-test is ordered after the heavy checks, so it lives there).

# Fast inner loop (cargo direct; do not satisfy the commit gate)
just fmt   [target]         # apply formatting
just test  [target]         # cargo test
just clippy [target]        # cargo clippy

# Per-crate target (test/clippy): core, db, api, api-client, ingestion,
# workers, dev, integrations, web, analysis
#   e.g. just clippy core   → cargo clippy -p chronoscope-core -- -D warnings

# Concrete actions
just web-dev [subset]       # integrated dev server over a facts-DB clone (default curated; auto-picks free ports)
just openapi                # build the raw OpenAPI spec (inspect the contract)
just xcodegen               # splice store paths into the xcodegen spec, regenerate the Xcode project
just corpus-hash            # add hashes for new corpus URLs
just model-test             # tests that load a real model, against realized artifacts
                            #   (needs the ONNX exports; not part of the gate)
just infra-plan             # compile the terranix modules, show what OpenTofu would change
just infra-apply            # apply them (real cloud resources; type it yourself)
just deploy                 # build+push the API image, deploy Cloud Run by digest
just deploy-web             # build the web bundle, publish it to the Cloudflare Worker
```

### Container image

`packages.oci-api` (Linux systems only) is the API server as an OCI image,
built with nix2container: the derivation output is a manifest over store
paths, so a build costs a JSON file and a push moves only the layers the
registry lacks. Layers split by rate of change: the C runtime closure
(glibc/openssl/sqlite/libspatialite + geo stack), then the baked curated
facts DB, then the rootfs, then the binary alone on top. Build it
cross-system from a dev machine:

```bash
nix build .#packages.x86_64-linux.oci-api
```

`checks.oci-api-boots` (also Linux-only) builds that image and runs its
entrypoint under the image's own environment, so a container that cannot
start fails a check rather than a deploy. `nix flake check` only evaluates
the current system, so on a darwin dev machine it never runs; `just check
linux` builds it.

### Infrastructure

Cloud resources are declared with terranix (Nix modules compiled to the
`config.tf.json` OpenTofu reads) in `nix/infra.nix`, and applied with
`just infra-apply`. `nix/infra-settings.nix` is the single definition of the
project's cloud coordinates: the declarations build from it and `just deploy`
reads it back, so the registry a push targets is the registry that was
declared. The state bucket is created once by hand, since state describing the
bucket would have to live in it; the justfile's infra section carries that
command.

The Cloud Run service is declared there too, down to its environment, its
runtime service account and its startup probe. The image is the one part a
deploy moves: `just deploy` builds it, pushes it, and hands the digest to
`tofu apply` as a variable, so a single tool owns the service and the
declaration keeps describing what is running.

### The front door

`chronoscope.io` is a Cloudflare Worker that serves the web bundle as static
assets and proxies `/api/*` to Cloud Run with the mount stripped. Assets that
match a file in the bundle are answered before the script runs, so only `/api`
costs an invocation. That single origin is what removes CORS from the app and
gives a passkey one hostname to bind to; `www` redirects to the apex, since the
API checks WebAuthn origins against the apex exactly. Workers rather than Pages
because Queue consumers, Image Resizing and Cron Triggers are Workers-only and
the media pipeline wants all three.

The Worker is declared alongside Cloud Run in `nix/infra.nix`, with the script
itself in `nix/front-door.js` and the bundle arriving as a variable the way the
image digest does. `just deploy-web` builds `packages.web` and applies; the
provider uploads the directory, sending only the files that changed. The
Cloudflare API token lives in Secret Manager and is read into the environment
per run, so it never reaches the state or the repo.

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
It also always derives `DateWalk` (the `UncertainDate` role-tagging
`visit_dates` walk): `#[date_role = "..."]` tags each direct `UncertainDate`
field (an untagged one fails to compile), recursion into an
`R: IdScheme`-parametrized composite is automatic, and `#[traverse]` is the
escape hatch that recurses into the few non-`R` date hosts (the citation
sources). A dateless type gets an empty `visit_dates`, so a `#[traverse]`
always lands on a type that has the method.
Variants and structs must use named fields — a tuple/newtype variant is a
compile error, since internal tagging flattens an unnamed payload beside
the tag and collides. Add the comparison/`Debug`/`Clone` derives yourself
in a `#[derive(..)]` alongside (float-bearing types carry hand-written
`Eq`/`Hash`/`Ord`). A type generic over `R: IdScheme` additionally gets the
`IdWalk` derive and the uniform `R: IdScheme` serde/schemars bounds emitted
for free — don't repeat them; validated products that can't be
`#[grammar_type]` (e.g. `GapBounds`) spell `#[derive(IdWalk)]` (and
`#[derive(DateWalk)]` when the date walk must reach them) themselves.
Exemptions: `Location`/`UnresolvedLocation` keep hand-written `Deserialize`;
the transparent leaf newtypes (ids, validated strings) use the `*_newtype!`
macros.

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