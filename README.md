# Chronoscope

A spatiotemporal knowledge platform - think "Wikipedia for places through time." Chronoscope transforms scattered historical photos, maps, and documents into an explorable timeline of any location, with humans and AI collaborating to resolve mysteries through photogrammetry and detective work.

## Goals

Chronoscope is building a **knowledge graph, not a photo gallery**. Photos, maps, and documents are evidence for assertions about how places evolved - not the end product. The platform models uncertainty explicitly (vague dates like "sometime in the 1920s" are first-class), requires citations for all assertions, and uses AI to assist human researchers rather than replace them.

The platform is **API-first**: the iOS app, eventual web frontend, browser extension, Android client, and command-line tools are all API clients. This makes it easy for organizations like historical societies to ingest their existing datasets.

See [docs/design.md](docs/design.md) for detailed project tenets.

## Current Status

This repo contains a **thin end-to-end slice** proving out the core flow:

- **Research URLs**: Submit URLs of historical content, which are fetched and analyzed by background workers
- **Passkey Authentication**: WebAuthn-based passwordless auth with stateless challenge flow
- **iOS App**: SwiftUI client with share extension for easy URL capture from Safari, social media, etc.

## Roadmap

The thin slice establishes the patterns for the full platform. Next steps include:

- **Entity model**: Buildings, streets, and landmarks as abstract entities separate from evidence
- **Transitions**: Track changes over time (constructed, modified, demolished) rather than static states
- **Evidence chains**: Link assertions to sources with machine-checkable citations
- **External integrations**: Wikidata, Library of Congress, OpenStreetMap, archive.org

## Getting Started

### Prerequisites

- [Nix](https://nixos.org/download/) ([Determinate Nix](https://determinate.systems/nix/) recommended)
- Xcode 16+
- ngrok (authenticated - `ngrok config add-authtoken YOUR_TOKEN`)

### Development Environment

All Rust, Python, and native dependencies are managed by Nix. Three shell tiers provide increasing levels of data so the base shell starts fast:

| Shell | What it adds | Use case |
|-------|-------------|----------|
| `default` | Rust + Python + lint tools | Web dev, API work, most of the repo |
| `analysis` | + model weights (DINOv3, SAM3) | Analysis pipeline tests |
| `corpus` | + corpus images | Full corpus test suite |

**Enter the dev shell:**

```bash
# Option 1: direnv (recommended — auto-activates on cd)
direnv allow

# Option 2: manual
nix develop              # default shell — no large downloads
nix develop .#analysis   # requires fetch-weights (see below)
nix develop .#corpus     # requires fetch-weights + fetch-corpus
```

`just` commands auto-wrap with the appropriate shell tier, so you can always just run `just check` directly.

**Fetching model weights and corpus data:**

Model weights (~2 GB) require a one-time fetch with a [HuggingFace token](https://huggingface.co/settings/tokens) that has access to the gated repos:

```bash
HF_TOKEN=hf_... just fetch-weights   # DINOv3 + SAM3 model weights
just fetch-corpus                     # corpus images from external URLs
just fetch-all                        # both of the above
```

Once fetched, data is pinned as GC roots in `.nix-gc-roots/` so Nix garbage collection won't sweep it. The HF token is only needed for the initial fetch.

### Quick Start

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
   This starts ngrok, updates `ios/Local.xcconfig` with the tunnel URL, and runs the API with background workers.

3. **Build and run in Xcode**: Open `ios/Chronoscope.xcodeproj` and hit `Cmd+R`.

That's it - one terminal command and one Xcode shortcut for a fully functioning system on a real device.

See [docs/development.md](docs/development.md) for detailed development practices.

### Running Checks

```bash
# The commit gate: runs everything — Nix lint, Rust (fmt/clippy/test/coverage),
# Python (ruff/mypy/pytest), and the headless-browser web tests. The targeted
# `just check <rust|web|triton|nix>` subsets are for iteration and skip parts of
# the gate (the browser suite runs only here), so run the full `just check`
# before committing.
just check

# Auto-fix all formatting (Nix + Rust + Python)
just fmt

# Hermetic sandboxed checks (no GPU required)
nix flake check

# Run a single Nix check (e.g., Python tests only)
nix build .#checks.$(nix eval --impure --expr builtins.currentSystem --raw).triton-test

# iOS UI tests
xcodebuild test -scheme Chronoscope -destination 'platform=iOS Simulator,name=iPhone 16'
```

## Project Structure

```
analysis/            Image analysis pipeline (Triton gRPC client, SAM3/VLM)
  triton/            Python model definitions for Triton Inference Server
api/                 Rust API server (Dropshot framework)
db/                  Database layer (sqlx + SQLite)
dev/                 Development server with ngrok integration
integrations/        Domain-specific integrations (Reddit, Instagram), HTTP client
workers/             Background workers (URL fetching, content extraction, analysis)
ios/
  App/               Main iOS app (SwiftUI)
  Shared/            Code shared between app and extension
  ShareExtension/    Share extension for URL capture
  ChronoscopeAPI/    Generated OpenAPI client package
```

See [docs/architecture.md](docs/architecture.md) for technical details.

## AI Use

This project was built with the help of AI - as a tool, not a shortcut. All code is reviewed, tested, and validated. What matters is whether the software works, solves real problems, and is maintainable.

AI-assisted contributions are welcome, but they need to pass human review, be well factored, and have other markers of quality code. Don't just tell your AI assistant to read an issue and fix it. Slop PRs will be closed.

## License

MIT
