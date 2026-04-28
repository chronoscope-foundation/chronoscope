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

- `REGIONS_DB` — path to the SpatiaLite regions database. Required.
- `SPATIALITE_LIBRARY_PATH` — directory containing libspatialite. Required;
  loaded at runtime via `SELECT load_extension()` for region assignment +
  point-in-polygon queries. Outside the shell, spatial queries fail at
  runtime, not compile time.
- `WIKIDATA_TEST_DB` — path to a curated test database (dev only).

Both `REGIONS_DB` and `SPATIALITE_LIBRARY_PATH` are set automatically by
`devShells.api` (and by the `mkApi`-wrapped binaries), and threaded into
the hermetic `test` / `llvm-cov` flake checks via `testExtraEnv` in
`nix/rust.nix`.

## Tests use real DB + simulated passkeys

In-memory SQLite per test. Passkeys are simulated (no real authenticator).
Production runs Postgres — **don't introduce single-writer assumptions**
that work in SQLite but break under Postgres concurrency.

## Italy / world variants

After the dev-UX refactor:

- `nix/api.nix` exposes `mkApi { regions }` — the function shape.
- `flake.nix` instantiates two: `packages.api-italy` (dev default, ~2GB)
  and `packages.api-world` (prod, ~86GB).
- Adding a new variant (e.g. EU-only) is two changes: a new entry in
  `nix/regions.nix` (using `mkRegions`), and a named instance in
  `flake.nix`'s `packages` block.
