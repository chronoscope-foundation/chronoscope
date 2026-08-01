# chronoscope-web

WASM frontend. Loaded automatically when working under `web/`.

## The wasm32 target gotcha

`chronoscope-web` is excluded from `default-members` in the workspace
`Cargo.toml` because cargo can't mix native and WASM targets in one
invocation. **A workspace-level `cargo check` does not check it.**

The fast inner loop:

- `just clippy web` — clippy with `-D warnings` twice: against
  `wasm32-unknown-unknown` for the shipping code, then `--all-targets` against
  the host, which is where the crate's `#[cfg(test)]` code builds and so the
  only place the workspace lint denials reach it.
- `just test web` — browser tests (needs `test-hooks` cargo feature + a
  pre-built `WEB_DIST`; both supplied by `devShells.web`). This is the inner
  loop for the browser suite.
- `just check web` — hermetic build checks only (web-build + web-test-build +
  web-clippy + web-native-test + web-native-clippy). The browser-test *run*
  (`web-test`) is **not** here: it's ordered after the heavy checks to avoid
  CPU starvation, so it runs only in the full `just check`. Iterate with
  `just test web`.

## OpenAPI-first

The `chronoscope-api-client` crate is hand-written Rust (the typed
`Client`/`AuthClient`), updated by hand to track the contract. When an
`api/src/` change alters the contract:

1. `just openapi` — build the raw spec (`packages.openapi`) to see the new contract
2. Update `chronoscope-api-client` by hand to match
3. Then this crate compiles against the new types

If the web crate fails to compile after an API change, suspect the client
wasn't updated to match.

## Production build pipeline

`trunk serve` is for dev iteration. The **production** pipeline (what
`packages.web` and `packages.web-test` build) is:

```
crane (wasm32) → wasm-bindgen → wasm-opt -Oz → tailwindcss → assemble dist/
```

Defined in `nix/web.nix` — Cargo configuration alone won't reproduce it.
If you need to alter how the bundle is shaped (e.g. different wasm-opt
flags, additional asset processing), edit `mkDist` in `nix/web.nix`.

## Serving topology (same-origin)

The web app and the API are served **same-origin**: a front door serves the
static bundle and reverse-proxies `/api/*` to the API (Dropshot), which owns its
routes at root (`/entities`, `/markers`, …). The front door strips the `/api`
mount, and the web client's base is `window.location.origin + "/api"`, so the
browser never makes a cross-origin request and no in-app CORS is needed.

Three environments implement that one front-door role with **different servers**,
because their needs diverge — same role, not redundancy:

| Environment | Front door | Proxy mechanism |
|---|---|---|
| `web-dev` (dev iteration) | **Trunk** (`trunk serve`) | built-in `--proxy-backend` + `--proxy-rewrite=/api/` |
| browser tests (`dev/tests/harness`) | minimal **axum** `ServeDir` | hand-written `proxy_api` handler (`ServeDir` can't proxy) |
| production | **Cloudflare Worker** (`nix/front-door.js`) | `fetch()` to the Cloud Run URL with `/api` sliced off |

Dev uses Trunk for its live-rebuild + auto-reload. The browser tests can't use
Trunk — they serve a fixed, hermetic prebuilt `WEB_DIST`, not a live rebuild — so
they use a lightweight axum static server plus a small reverse-proxy. Prod is
Cloudflare. Each is the same "serve static + proxy `/api`" shape in the tool that
fits its environment.

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
