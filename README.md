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

- Rust
- Xcode 16+
- [XcodeGen](https://github.com/yonaskolb/XcodeGen)
- ngrok (authenticated - `ngrok config add-authtoken YOUR_TOKEN`)

### Quick Start

1. **Set up the iOS project** (one-time):
   ```bash
   cd ios
   cp Local.xcconfig.example Local.xcconfig
   # Edit Local.xcconfig with your Apple Developer Team ID
   xcodegen generate
   ```

2. **Start the dev server**:
   ```bash
   cargo run -p chronoscope-dev
   ```
   This starts ngrok, updates `ios/Local.xcconfig` with the tunnel URL, and runs the API with background workers.

3. **Build and run in Xcode**: Open `ios/Chronoscope.xcodeproj` and hit `Cmd+R`.

That's it - one terminal command and one Xcode shortcut for a fully functioning system on a real device.

See [docs/development.md](docs/development.md) for detailed development practices.

### Running Tests

```bash
# API tests (includes auth flow with simulated passkeys)
cargo test

# iOS UI tests
xcodebuild test -scheme Chronoscope -destination 'platform=iOS Simulator,name=iPhone 16'
```

## Project Structure

```
api/                 Rust API server (Dropshot framework)
db/                  Database layer (sqlx + SQLite)
workers/             Background workers (URL fetching, content extraction)
dev/                 Development server with ngrok integration
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
