# Architecture

This document describes the technical architecture of Chronoscope.

## Overview

```
                      ┌───────────────────┐
   browser ──────────>│ Cloudflare Worker │  static assets, /api/* proxy
                      └─────────┬─────────┘
                                │
   iOS app ────────────────────>│
  (unmaintained)                v
                      ┌───────────────────┐
                      │     Rust API      │  Dropshot, OpenAPI-first
                      │    (Cloud Run)    │
                      └─────────┬─────────┘
                                │
                 ┌──────────────┴──────────────┐
                 v                             v
         [app database]                 [facts database]
         users, research,               the fact store
         media, work queue              (attached as `ovl`)
                 │
                 v
           [Workers]
           URL fetch, content extraction
```

The API is the single source of truth for business logic. The frontend and the
API share one origin: the Worker answers for static assets and forwards
`/api/*` to Cloud Run with the mount stripped, so the browser never makes a
cross-origin request. Background workers process asynchronous tasks like URL
fetching and content extraction.

## API Server

**Framework**: [Dropshot](https://github.com/oxidecomputer/dropshot) (Oxide's OpenAPI-first REST framework)

**Key modules:**
- `auth.rs` - WebAuthn registration and login
- `entities.rs` - The entity read surface: listing, detail, depicting images, and the `/tiles/{z}/{x}/{y}` clustering endpoint
- `entity_types.rs` - Wire types for that surface, shared with the client
- `mirror/` - Copying source images into owned storage, and the sweep that drives it
- `media.rs`, `cdn.rs` - Media serving and cache headers
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

**Engine**: SQLite via sqlx, with SpatiaLite for the spatial index. A Postgres
fact-store backend is in progress; `db/src/sqlite/` and `db/src/postgres/`
implement the same store behind one interface.

There are two schemas, migrated separately under `db/migrations/`. The app
schema is `main` at serve time and the fact store attaches beside it as `ovl`,
so ingestion can build a facts database offline and have it swapped in without
touching the app's tables.

### App schema (`db/migrations/app/`)

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

### Fact schema (`db/migrations/facts/`)

- `fact_commits`, `facts` - Content-addressed commits and the facts they carry. Rows are never deleted; a retraction is another fact.
- `fact_counters`, `fact_subjects`, `subject_reps` - Subject id minting, and the representative a subject resolves to after merges and splits
- `facts_spatial` - The SpatiaLite index behind marker and tile queries
- `existence_witness`, `event_witness`, `has_event`, `construction_start`, `demolition_completed` - Witness tables the read path scans per entity, each row carrying the fact it came from so liveness is decided per edge

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

### Analysis

**No analysis worker runs today.** The previous one drove a Triton Inference
Server over gRPC, with the models defined in Python; both are gone, and
extracted images sit at `Pending`.

The replacement runs the models in-process instead of behind a server, which
is what lets the same binary work on a laptop and in production: SAM 3 and
DINOv3 as ONNX graphs exported by Nix (`nix/vision.nix`) and executed through
ONNX Runtime, and Qwen 3.6 through mistral.rs. That work lives in the
`analysis` crate, which carries the model runners, the detection pipeline, and
the corpus machinery: the pinned test images the pipeline is developed against.

The `analyze` binary is how it runs today. The queue worker follows, on the
same work-queue pattern as the URL fetcher.

## iOS App

**Not currently maintained.** The app has not been updated since the fact store
landed, so what follows describes its shape rather than a working client. The
README carries the revival plan.

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

The API contract flows from Rust to Swift:

1. Dropshot macros + schemars derives define the API in Rust
2. The `openapi` binary serializes the spec (`packages.openapi`), which is assembled into the ChronoscopeAPI SwiftPM package (`packages.ios-api-package`)
3. `just xcodegen` splices that package's store path and the Swift tool paths into the xcodegen spec (`packages.ios-project-spec`, via `replaceVars`), pins it so the closure survives GC, and generates `ios/Chronoscope.xcodeproj` in place with those paths baked in
4. The Swift OpenAPI Generator plugin produces client code from the package at Xcode build time

The spec is content-addressed in the store; `just openapi` builds the raw contract when you want to inspect it. After changing the Rust API (or the hand-written sources under `ios/ChronoscopeAPI/`), re-run `just xcodegen`.

## Deployment

The API ships as an OCI image built by nix2container (`packages.oci-api`),
layered by rate of change so a push moves only what changed: the C runtime
closure, then the baked curated facts DB, then the rootfs, then the binary.
`checks.oci-api-boots` runs the entrypoint under the image's own environment,
so a container that cannot start fails a check rather than a deploy.

`chronoscope.io` is a Cloudflare Worker serving the web bundle as static assets
and proxying `/api/*` to Cloud Run. Assets matching a file in the bundle are
answered before the script runs, so only `/api` costs an invocation. That
single origin is what removes CORS from the app and gives a passkey one
hostname to bind to.

Both the service and the Worker are declared with terranix in `nix/infra.nix`
and applied with OpenTofu. The image digest is the one part a deploy moves:
`just deploy` builds it, pushes it, and hands the digest to `tofu apply`, so a
single tool owns the service and the declaration keeps describing what is
running.

## Development Server

`just web-dev` is the loop for the web frontend. It realizes a facts database,
starts the API on a free port, and runs Trunk in front of it with `/api`
proxied through, giving the dev server the same single-origin shape the
production front door has.

`cargo run -p chronoscope-dev` is the iOS-facing variant:

1. Finds available port
2. Starts ngrok tunnel (required for passkeys - must be HTTPS with stable domain)
3. Updates `ios/Local.xcconfig` with tunnel URL
4. Spawns URL fetcher workers
5. Runs API server with embedded media serving

The iOS app reads `API_SERVER_DOMAIN` from xcconfig, so rebuilds automatically pick up the current tunnel.
