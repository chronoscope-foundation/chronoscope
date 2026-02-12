# Architecture

This document describes the technical architecture of Chronoscope.

## Overview

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

All clients communicate with the Rust API over HTTPS. The API is the single source of truth for business logic. Background workers process asynchronous tasks like URL fetching and content extraction.

## API Server

**Framework**: [Dropshot](https://github.com/oxidecomputer/dropshot) (Oxide's OpenAPI-first REST framework)

**Key modules:**
- `auth.rs` - WebAuthn registration and login
- `research.rs` - URL submission, listing, details
- `users.rs` - Profile management, following URLs
- `jwt.rs` - Session token generation and validation
- `url_security.rs` - SSRF protection (DNS resolution, private IP blocking)

**Patterns:**
- Endpoints use `RequestContext<Arc<AppState>>` for dependency injection
- Request/response types derive `JsonSchema` for automatic OpenAPI generation
- Stateless design - no server-side session storage

## Authentication

Passwordless authentication using WebAuthn passkeys:

### Registration Flow
1. Client calls `POST /auth/register/start` with username
2. Server returns challenge with signed state (no server-side storage)
3. Client creates passkey credential via platform authenticator
4. Client calls `POST /auth/register/finish` with credential
5. Server verifies signature, stores credential, returns JWT

### Login Flow
1. Client calls `POST /auth/login/start` with username
2. Server returns challenge with signed state
3. Client signs challenge with passkey
4. Client calls `POST /auth/login/finish` with assertion
5. Server verifies, returns JWT

**Why stateless challenges?** The challenge state is signed and sent to the client, avoiding server-side session storage. This simplifies horizontal scaling and eliminates a class of bugs.

## Database

**Engine**: SQLite via sqlx (API compatible with Postgres for future migration)

### Schema Highlights

**Users & Credentials:**
- `users` - id, username, email, created_at
- `credentials` - WebAuthn passkeys stored as serialized JSON

**Research:**
- `research_urls` - Work queue with status tracking (pending → processing → complete/failed)
- `pages` - Extracted content (title, author, markdown body)
- `media` - Deduplicated by content hash, with EXIF extraction (GPS, capture date)
- `page_media` - Junction table linking pages to media

**Social:**
- `follows` - Users following research URLs

### Query Safety

All queries are defined in `db/src/queries.rs` and verified at startup:

```rust
define_queries! {
    GET_USER: "SELECT ... FROM users WHERE id = ?",
    // ...
}
```

On startup, each query runs through `EXPLAIN QUERY PLAN` to verify index usage. The application refuses to start if any query would do a full table scan.

## Background Workers

**Architecture**: Database-backed work queue with optimistic locking

### Work Queue Pattern

1. Worker claims batch of pending items (sets `claimed_at`, `claimed_by`)
2. Worker processes items
3. Worker updates status (complete or failed with retry)
4. Failed items retry with exponential backoff

**Why optimistic locking?** No distributed lock coordination needed. Workers can scale horizontally. Claimed items that aren't completed (worker crash) are automatically reclaimed after timeout.

### URL Fetcher Worker

Fetches URLs and extracts content:

**Generic extraction:**
- HTML → readability extraction → markdown conversion
- Images → EXIF extraction, perceptual hashing, thumbnail generation
- Videos → metadata extraction (duration, dimensions)

**Domain-specific fetchers:**
- Reddit: Extract post metadata, comments, linked media
- Instagram: Batch media extraction via Apify integration

**Content deduplication:**
- Exact hash for byte-identical content
- Perceptual hash for near-duplicate images

### Analysis Worker

Processes extracted images through a Triton Inference Server pipeline:

1. **SAM3** - Segments entities (buildings, landmarks) in images, produces confidence-filtered masks
2. **VLM** - Analyzes scenes and segmented regions, extracts structured descriptions
3. **Embeddings** - Generates embeddings for whole images and individual entities

The `analysis` crate provides a gRPC client for the Triton server, while the Python model definitions live in `analysis/triton/`. The analysis worker uses the same work queue pattern as the URL fetcher.

## iOS App

**Framework**: SwiftUI with Swift 6

### Structure

- `App/` - Main application screens
- `Shared/` - Code shared with share extension (API client, auth manager)
- `ShareExtension/` - URL capture from other apps
- `ChronoscopeAPI/` - Generated OpenAPI client (SPM package)

### Key Patterns

**API Protocol:**
```swift
protocol APIProtocol {
    func submitResearch(...) async throws -> Response
    // ...
}
```

Real client and mock client both conform, enabling:
- SwiftUI previews with mock data
- UI tests without network
- Dependency injection in production

**Authentication:**
- `AuthManager` handles passkey lifecycle via AuthenticationServices
- JWT tokens stored in Keychain
- `AuthenticatingMiddleware` injects Bearer token into requests

**Share Extension:**
- Captures URLs from Safari, social media, etc.
- Shares Keychain access group with main app for authentication
- Uses `SharedConfig` (via App Groups) for API domain

## OpenAPI Integration

The API contract flows from Rust to Swift automatically:

1. Dropshot macros + schemars derives define the API in Rust
2. `cargo run --bin openapi -- api/target/openapi.json` generates the spec
3. iOS project symlinks to this file at `ios/ChronoscopeAPI/Sources/ChronoscopeAPI/openapi.json`
4. Swift OpenAPI Generator plugin generates client code at Xcode build time
5. Xcode pre-build phase regenerates spec if Rust sources changed

This means API changes are immediately reflected in the Swift client - no manual sync needed.

## Development Server

`cargo run -p chronoscope-dev` provides an integrated development environment:

1. Finds available port
2. Starts ngrok tunnel (required for passkeys - must be HTTPS with stable domain)
3. Updates `ios/Local.xcconfig` with tunnel URL
4. Spawns URL fetcher workers
5. Runs API server with embedded media serving

The iOS app reads `API_SERVER_DOMAIN` from xcconfig, so rebuilds automatically pick up the current tunnel.
