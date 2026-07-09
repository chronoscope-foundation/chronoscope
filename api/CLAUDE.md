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

It is set automatically by `devShells.api` (and baked into the wrapped
`packages.api` binary), and threaded into the hermetic `test` / `llvm-cov`
flake checks via `testExtraEnv` in `nix/rust.nix`.

## Tests use real DB + simulated passkeys

In-memory SQLite per test. Passkeys are simulated (no real authenticator).
Production runs Postgres — **don't introduce single-writer assumptions**
that work in SQLite but break under Postgres concurrency.
