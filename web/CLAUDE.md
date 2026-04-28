# chronoscope-web

WASM frontend. Loaded automatically when working under `web/`.

## The wasm32 target gotcha

`chronoscope-web` is excluded from `default-members` in the workspace
`Cargo.toml` because cargo can't mix native and WASM targets in one
invocation. **A workspace-level `cargo check` does not check it.**

The fast inner loop:

- `just clippy web` — clippy against `wasm32-unknown-unknown` with `-D warnings`
- `just test web` — browser tests (needs `test-hooks` cargo feature + a
  pre-built `WEB_DIST`; both supplied by `devShells.web`)
- `just check web` — hermetic version (web-build + web-test-build + web-clippy)

## OpenAPI-first

The `chronoscope-api-client` crate is generated from the OpenAPI spec.
Touching anything in `api/src/` that changes the contract requires:

1. `just openapi` — regenerates `api/target/openapi.json`
2. Regenerate the client (typed `Client`/`AuthClient`)
3. Then this crate compiles against the new types

If the web crate fails to compile after an API change, suspect step 1 or 2
hasn't been run.

## Production build pipeline

`trunk serve` is for dev iteration. The **production** pipeline (what
`packages.web` and `packages.web-test` build) is:

```
crane (wasm32) → wasm-bindgen → wasm-opt -Oz → tailwindcss → assemble dist/
```

Defined in `nix/web.nix` — Cargo configuration alone won't reproduce it.
If you need to alter how the bundle is shaped (e.g. different wasm-opt
flags, additional asset processing), edit `mkDist` in `nix/web.nix`.

## Browser tests

Live in `dev/tests/web.rs`. Run via `just test web` (or `just web-test`'s
successor: `just test web` is the new entry point).

Three prerequisites that fail silently if missing:

- `test-hooks` cargo feature on `chronoscope-web` (the wasm bundle needs
  the instrumentation it adds) — wired into `devShells.web`'s
  `WEB_DIST = web.packages.web-test`.
- `WEB_DIST` env var must point at the prebuilt test bundle — set by
  `devShells.web`.
- `browser-tests` cargo feature on `chronoscope-dev` (gates the test
  target itself; without the feature, cargo treats `tests/web.rs` as
  non-existent). The `just test web` recipe enables this automatically.

The `browser-tests` gate exists so the hermetic workspace `test` flake
check (which can't supply a system Chrome) silently skips browser tests
rather than failing them. The WASM build itself is still covered by
`web-build` / `web-test-build` flake checks; only the browser-driven
E2E layer is gated this way.
