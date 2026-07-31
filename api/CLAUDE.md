# chronoscope-api

API server. Dropshot + schemars + WebAuthn passkeys. Loaded automatically
when working under `api/`.

## OpenAPI-first

The OpenAPI spec is the contract. Endpoint definitions are Dropshot
macros + schemars-derived schemas; the spec is generated from them, not
hand-written.

Endpoint workflow:

1. Edit the macro / handler in this crate
2. `just openapi` — build the raw spec (`packages.openapi`) to inspect the contract
3. Refresh downstream clients — iOS via `just xcodegen` (splices the spec into
   the ChronoscopeAPI package and regenerates the Xcode project); the web/CLI
   clients are hand-wired
4. Now the rest of the workspace compiles against the new contract

The `openapi` binary (`api/src/bin/openapi.rs`) writes the JSON; the iOS client
reads it via the generated ChronoscopeAPI package.

## Runtime env vars

The API server reads at startup:

- `SPATIALITE_LIBRARY_PATH` — directory containing libspatialite. Required;
  loaded into every SQLite connection at pool creation. Outside the shell,
  database startup fails at runtime, not compile time.
- `PORT` / `BIND_ADDR` — where to listen. `PORT` (what Cloud Run injects) wins
  and binds every interface; `BIND_ADDR` carries a full address otherwise.

`nix/api.nix` owns the server's runtime env as one attrset: the wrapped
`packages.api` binary, the `packages.oci-api` image config, `devShells.api`,
and the hermetic `test` / `llvm-cov` checks (via `testExtraEnv` in
`nix/rust.nix`) all derive from it, so a new variable reaches every launch
path at once.

## Serving lifecycle

Two pieces exist for the container deployment (`nix/oci.nix`, `just deploy`):

- `GET /health` (`health.rs`) — unauthenticated readiness. The status code is
  the whole contract: 204 with no body, or 503. Checks a connection out of the
  app pool and takes a fact-store snapshot, so a server that bound its port
  without usable stores answers 503. A short probe budget caps both checks, and
  exhausting it still answers 204, which keeps a platform from recycling the
  instance carrying the most traffic. Point a platform startup probe at it; the
  default TCP probe cannot see either failure.
- SIGTERM in `main.rs` drains through `HttpServer::close`, which is what keeps
  both pool closes (and SpatiaLite's `dlclose`) inside the live runtime under a
  container runtime's shutdown. The drain is bounded and isolated on its own
  task, so the pool closes are reachable whether it finishes, panics, or runs
  out of time.

## Tests use real DB + simulated passkeys

In-memory SQLite per test. Passkeys are simulated (no real authenticator).
Production runs Postgres — **don't introduce single-writer assumptions**
that work in SQLite but break under Postgres concurrency.
