# Chronoscope

## Vision

Chronoscope aims to be a spatiotemporal knowledge platform - think "Wikipedia for places through time." The goal is to transform scattered historical photos, maps, and documents into an explorable timeline of any location, with users and "tasteful AI" collaborating to resolve mysteries through photogrammetry and detective work.

### Core Principles

- **Knowledge graph, not photo gallery**: Photos are evidence for assertions about how places evolved, not the end product
- **Wiki model**: Crowdsourced with self-correcting mechanisms, evidence-based assertions, transparent history
- **Citations everywhere**: Knowledge doesn't exist in Chronoscope without attribution, with varying degrees of machine-checkable citations
- **Uncertainty as feature**: First-class vague dates ("sometime in the 1920s") and locations ("near Main St") invite refinement
- **Graceful degradation**: AI provides hints, humans make decisions; platform works even if automation fails

### Future Data Model (Not Yet Implemented)

The full vision includes:
- **Entities**: Buildings, streets, landmarks exist abstractly separate from evidence
- **Transitions**: Track changes (constructed, modified, demolished) not static states
- **Evidence chains**: Every assertion requires sources
- **Integrations**: Wikidata, Library of Congress, OpenStreetMap, archive.org

## Current Implementation

This repo contains a "thin end-to-end slice" to prove out the core flow:

### What's Built

1. **Research URLs**: Users submit URLs (of historical images, documents, etc.) and can follow them
   - This is the minimal content type that will eventually feed into the full entity/evidence system
   - URLs are validated (SSRF protection) and fetched by background workers
   - Domain-specific fetchers (e.g., Reddit) extract structured content and media

2. **Passkey Authentication**: WebAuthn-based passwordless auth
   - Stateless challenge flow (signed state sent to client)
   - JWT session tokens

3. **iOS App**: Native client with share extension
   - Easy URL capture from Safari, social media, etc.
   - Passkey registration and login

### Architecture

```
┌─────────────┐     OpenAPI      ┌─────────────┐
│   iOS App   │ ←───(generated)──│  Rust API   │
│  (SwiftUI)  │                  │ (Dropshot)  │
└─────────────┘                  └─────────────┘
       │                                │
       │ WebAuthn                       │ SQLite
       ▼                                ▼
   [Passkeys]                      [Database]
```

**API** (`api/`):
- Dropshot framework (Oxide's REST framework with OpenAPI generation)
- SQLite via sqlx (same API works for Postgres later)
- WebAuthn via webauthn-rs
- JWT sessions via jwt-compact

**iOS** (`ios/`):
- SwiftUI with Swift 6
- Swift OpenAPI Generator for type-safe API client
- XcodeGen for project generation
- Share extension for URL ingestion

## Code Conventions

### Rust

- `#![deny(clippy::unwrap_used)]` - no unwraps, handle errors properly
- OpenAPI-first: endpoints defined with Dropshot macros, schema via schemars
- Tests use real database with in-memory SQLite and webauthn-authenticator-rs for passkey simulation

### Swift

- SwiftFormat and SwiftLint enforced via build phases
- Mock API clients for previews and UI tests
- `APIProtocol` abstraction allows swapping real/mock implementations

## Development

### Running Locally

For iOS passkey testing, you need a public HTTPS URL (passkeys require secure context):

```bash
cargo run -p chronoscope-dev
```

This starts ngrok, automatically updates `ios/Local.xcconfig` with the tunnel domain, and runs the API server with background workers. Then use Xcode normally to build/run the iOS app.

### OpenAPI Flow

1. Xcode build phase runs `cargo run --bin openapi` to generate `api/target/openapi.json`
2. iOS project symlinks to this file (`ios/ChronoscopeAPI/Sources/ChronoscopeAPI/openapi.json`)
3. Swift OpenAPI Generator plugin generates client code at build time

### Testing

```bash
# API tests (includes auth flow with simulated passkeys)
cd api && cargo test

# iOS UI tests
xcodebuild test -scheme Chronoscope -destination 'platform=iOS Simulator,name=iPhone 16'
```
