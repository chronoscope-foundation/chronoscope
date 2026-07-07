# chronoscope-api

API server. Dropshot + schemars + WebAuthn passkeys. Loaded automatically
when working under `api/`.

## OpenAPI-first

The OpenAPI spec is the contract. Endpoint definitions are Dropshot
macros + schemars-derived schemas; the spec is generated from them, not
hand-written.

Endpoint workflow:

1. Edit the macro / handler in this crate
2. `just openapi` — regenerates `api/target/openapi.json`
3. Regenerate downstream clients (`chronoscope-api-client`, web frontend,
   future iOS Swift client)
4. Now the rest of the workspace compiles against the new contract

The spec lives in `api/src/bin/openapi.rs`'s output — the `openapi`
binary writes the JSON. iOS, web, and CLI clients all read from this.

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
