# Chronoscope

A crowdsourced platform for exploring how places change over time. Users collect and share historical photos, maps, and documents to build a collaborative timeline of any location.

> **Status**: Early development - implementing a thin end-to-end slice with URL collection and passkey authentication.

## What's Here

- **API** (Rust): REST API with WebAuthn passkey authentication, URL submission/following, SQLite storage
- **iOS App** (Swift): SwiftUI app with passkey login, research list, and share extension for easy URL capture

## Getting Started

### Prerequisites

- Rust
- Xcode 16+
- [XcodeGen](https://github.com/yonaskolb/XcodeGen)
- ngrok (authenticated - `ngrok config add-authtoken YOUR_TOKEN`)

### Development Workflow

1. **Set up the iOS project** (one-time):
   ```bash
   cd ios
   cp Local.xcconfig.example Local.xcconfig
   # Edit Local.xcconfig with your Apple Developer Team ID
   xcodegen generate
   ```

2. **Start the dev server** (in a terminal):
   ```bash
   cd api
   cargo run --bin dev
   ```
   This starts ngrok, updates `ios/Local.xcconfig` with the tunnel URL, and runs the API server.

3. **Build and run in Xcode**:
   Open `ios/Chronoscope.xcodeproj`, build and run normally (⌘R).

The iOS app reads the API domain from `Local.xcconfig`, so each rebuild picks up the current ngrok URL automatically. The URL can't be dynamic since the iOS passkey implementation expects the specific domain to be built into the signed app entitlements.

### Without ngrok

For API-only development (passkeys won't work from iOS):
```bash
cd api
export JWT_SECRET="dev-secret"
cargo run
```

### OpenAPI

The iOS client is generated from `api/target/openapi.json` (symlinked into the iOS project). The Xcode build phase regenerates it automatically when API source changes.

## Project Structure

```
api/                 Rust API server (Dropshot framework)
ios/
  App/               Main iOS app (SwiftUI)
  Shared/            Code shared between app and extension
  ShareExtension/    Share extension for URL capture
  ChronoscopeAPI/    Generated OpenAPI client package
  UITests/           UI tests
```

## AI Use

This project was built with the help of AI - as a tool, not a shortcut. All code is reviewed, tested, and validated. What matters is whether the software works, solves real problems, and is maintainable and easily understood by both humans and AI.

AI-assisted contributions are welcome, but they need to pass human review, be well factored, and have other markers of quality code. Don't just tell your AI assistant to read an issue and fix it. Slop PRs will be closed.

Disclosure format borrowed from [intercept](https://github.com/smittix/intercept/tree/ac0235312ca1e492caddbed2c5d2e7f4faccc4d9#ai-use).

## License

MIT
