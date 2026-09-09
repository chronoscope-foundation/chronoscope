# Chronoscope

A spatiotemporal knowledge platform - think "Wikipedia for places through time." Chronoscope transforms scattered historical photos, maps, and documents into an explorable timeline of any location, with humans and AI collaborating to resolve mysteries through photogrammetry and detective work.

## Goals

Chronoscope is building a **knowledge graph, not a photo gallery**. Photos, maps, and documents are evidence for assertions about how places evolved - not the end product. The platform models uncertainty explicitly (vague dates like "sometime in the 1920s" are first-class), requires citations for all assertions, and uses AI to assist human researchers rather than replace them.

The platform is **API-first**: the web frontend, iOS app, browser extension, Android client, and command-line tools are all API clients. This makes it easy for organizations like historical societies to ingest their existing datasets.

See [docs/design.md](docs/design.md) for detailed project tenets.

## Current Status

Chronoscope is live at [chronoscope.io](https://chronoscope.io).

The center of the system is a **fact store**. Every assertion is a fact with a
citation, sources are allowed to disagree, and solvers reconcile the
disagreement into the entity a reader actually sees. Around it:

- **Fact store** (`core/`, `db/`): the grammar, the solvers, and the backends it
  persists to. Commits are content-addressed; conflicting claims stay visible
  rather than being settled before storage.
- **Web frontend** (`web/`): a WASM map over the entity graph, with tiled
  clustering and a time slider that rewinds the map to a given year.
- **API** (`api/`): Dropshot, OpenAPI-first. Passkey auth, entity and tile
  endpoints, URL research, and the media mirror.
- **Bulk ingestion** (`ingestion/`): Wikidata's architectural entities, built
  and pinned through Nix.
- **Image analysis** (`analysis/`): SAM 3, DINOv3, and Qwen 3.6 running
  in-process through ONNX Runtime and mistral.rs, driven by the `analyze` CLI.
- **Workers** (`workers/`): URL fetching and content extraction on a
  database-backed work queue.

**The iOS app is expected to be broken.** It has not been updated since the
fact store landed, and the API contract moved underneath it. Reviving it is on
the roadmap; until then, treat `ios/` as unmaintained and don't take its
contents as a description of the current API.

## Roadmap

- **Analysis queue worker**: the models run today from the `analyze` CLI.
  Wiring them onto the work queue is what moves extracted images off `Pending`.
- **Postgres fact-store backend**: in progress alongside the SQLite one.
- **iOS revival**: regenerate the client against the current contract and bring
  the app back.

## Getting Started

### Prerequisites

- [Nix](https://nixos.org/download/) ([Determinate Nix](https://determinate.systems/nix/) recommended)

Everything else comes from Nix. iOS work additionally needs Xcode 16+ and ngrok
(`ngrok config add-authtoken YOUR_TOKEN`).

### Development Environment

All toolchains and native dependencies are managed by Nix. There is one shell
per project area, each sized to what that area needs, so entering the base
shell costs nothing:

| Shell | What it adds | Use case |
|-------|-------------|----------|
| `default` | Rust toolchain + just + Nix lint tools | Poking around, running `just <recipe>` |
| `api` | + sqlite/openssl/spatialite/protobuf | Backend / API server work |
| `web` | + wasm toolchain, trunk, tailwind, chromium | Frontend, `web-dev`, browser tests |
| `analysis` | + onnxruntime, corpus images | Analysis pipeline work |
| `ios` | xcodegen, swiftformat/swiftlint | Xcode project generation, Swift lint |
| `deploy` / `infra` | gcloud, skopeo / opentofu | Shipping the image, declaring cloud resources |

**Enter the dev shell:**

```bash
# Option 1: direnv (recommended — auto-activates on cd)
direnv allow

# Option 2: manual
nix develop              # default shell — no large downloads
nix develop .#analysis   # requires fetch-corpus (see below)
```

`just` commands auto-wrap with the appropriate shell tier, so you can always just run `just check` directly.

**Fetching model weights and data:**

Model weights (~2 GB) require a one-time fetch with a [HuggingFace token](https://huggingface.co/settings/tokens) that has access to the gated repos:

```bash
HF_TOKEN=hf_... just fetch-weights    # DINOv3 + SAM3 + Qwen model weights
just fetch-models [resolution]         # ONNX exports the vision crate loads
just fetch-corpus                      # corpus images from external URLs
just fetch-all                         # weights + corpus
just fetch-wikidata                    # the architectural-entities set
just fetch-wikidata-db [subset]        # a SQLite facts DB (curated|1k|100k|full)
```

Once fetched, data is pinned as GC roots in `.nix-gc-roots/` so Nix garbage collection won't sweep it. The HF token is only needed for the initial fetch.

### Quick Start

```bash
just web-dev
```

This builds the WASM bundle, realizes a facts database, and serves the frontend
with the API proxied behind it, picking free ports so it never collides with
another running copy. It is the loop for everything except iOS work.

### iOS

The app does not currently track the API contract (see Current Status). These
are the steps for when it does:

1. **Set up the iOS project** (one-time):
   ```bash
   cd ios
   cp Local.xcconfig.example Local.xcconfig
   # Edit Local.xcconfig with your Apple Developer Team ID
   just xcodegen
   ```

2. **Start the dev server**:
   ```bash
   cargo run -p chronoscope-dev
   ```
   This starts ngrok, updates `ios/Local.xcconfig` with the tunnel URL, and runs the API with background workers. Passkeys need a stable HTTPS domain, which is what the tunnel provides.

3. **Build and run in Xcode**: Open `ios/Chronoscope.xcodeproj` and hit `Cmd+R`.

See [docs/development.md](docs/development.md) for detailed development practices.

### Running Checks

```bash
# The commit gate: runs everything — Nix lint, Rust (fmt/clippy/test/coverage),
# and the headless-browser web tests. The targeted
# `just check <rust|web|nix>` subsets are for iteration and skip parts of
# the gate (the browser suite runs only here), so run the full `just check`
# before committing.
just check

# Auto-fix all formatting (Nix + Rust)
just fmt

# Hermetic sandboxed checks (no GPU required)
nix flake check

# Run a single Nix check (e.g., the Nix lint alone)
nix build .#checks.$(nix eval --impure --expr builtins.currentSystem --raw).nix-lint
```

## Project Structure

```
analysis/            Image analysis: SAM 3, DINOv3, Qwen 3.6 via ONNX Runtime and mistral.rs
api/                 Rust API server (Dropshot framework)
api-client/          Typed HTTP client and the shared contract types the web frontend uses
core/                The fact store: grammar, solvers, projections
db/                  Database layer (sqlx): app schema and fact-store backends
dev/                 Development servers (`web-dev`, and the ngrok flow for iOS)
ingestion/           Bulk ingestion from external knowledge bases (Wikidata)
integrations/        Domain-specific integrations (Reddit, Instagram), HTTP client
macros/              Procedural macros for the fact-store grammar
tools/quantize/      Pre-quantizes Qwen to a UQFF the analysis crate loads
web/                 WASM frontend (Leptos)
workers/             Background workers (URL fetching, content extraction)
ios/
  App/               Main iOS app (SwiftUI)
  Shared/            Code shared between app and extension
  ShareExtension/    Share extension for URL capture
  ChronoscopeAPI/    Generated OpenAPI client package
nix/                 Flake modules: dev shells, packages, checks, infrastructure
docs/                Design, architecture, and development practices
```

See [docs/architecture.md](docs/architecture.md) for technical details.

## Deployment

The API runs on Cloud Run as an OCI image built by Nix. `chronoscope.io` is a
Cloudflare Worker that serves the web bundle as static assets and proxies
`/api/*` to Cloud Run, which is what makes the app single-origin and gives
passkeys one hostname to bind to. Both the service and the Worker are declared
with terranix and applied with OpenTofu, so what is running is what the
repository describes. The root `CLAUDE.md` carries the recipes.

## AI Use

This project was built with the help of AI - as a tool, not a shortcut. All code is reviewed, tested, and validated. What matters is whether the software works, solves real problems, and is maintainable.

AI-assisted contributions are welcome, but they need to pass human review, be well factored, and have other markers of quality code. Don't just tell your AI assistant to read an issue and fix it. Slop PRs will be closed.

## License

The code is MIT. The knowledge graph is released under Creative Commons
Attribution 4.0, so the work stays usable no matter what happens to any one
organization. Chronoscope is a project of the Chronoscope Foundation.
