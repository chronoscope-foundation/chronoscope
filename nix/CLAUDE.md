# nix/

Nix build infrastructure. One derivation module per project area;
`flake.nix` composes them.

## Layout

| Module          | What it owns                                        |
|-----------------|-----------------------------------------------------|
| `rust.nix`      | Workspace `commonArgs`, native checks, `default` package |
| `api.nix`       | Wrapped `chronoscope-api` binary (SpatiaLite path baked in) |
| `web.nix`       | WASM build pipeline + `web-build`/`web-test-build`/`web-clippy` checks |
| `python.nix`    | `analysisEnv`, model weight FODs, triton checks     |
| `corpus.nix`    | Per-URL image FODs, link farm, `analysis-results` GPU derivation |
| `regions.nix`   | OSM PBF → SpatiaLite, italy + world variants        |
| `wikidata.nix`  | Curated Wikidata entity fetch → `entities.jsonl` FOD |

Dev shell composition lives in `flake.nix`, not in any single component
module — it has the visibility to compose across modules.

## The function-with-named-instances pattern

Where a derivation has variants that are interesting at the flake level
(not just internal), expose:

1. A function that takes the variant input (e.g. `mkRegions name pbf`).
2. Named instances in `flake.nix`'s `packages` (e.g. `regions-italy-db`,
   `regions-world-db`).

Adding a new variant is one entry in `flake.nix` — no new branches in
the function itself.

## Coverage rule for `just check`

`just check` is hermetic and equivalent to `nix flake check` (or a
focused subset). **Every gate the project commits behind must be a
flake check.** If you add a new check (e.g. a new lint, a new build),
expose it via `checks.<system>.<name>`, not just as a step in the
justfile — otherwise it slips past the pre-commit gate.

## GC root pinning

Large derivations (model weights, corpus images, regions DBs, the
wikidata entities snapshot) get pinned as GC roots in `.nix-gc-roots/`
(gitignored) so they survive `nix store gc`. Pinning happens in shell
hooks:

- `gcRootsPrelude` — creates `.nix-gc-roots/`
- `pinWikidataRoot`, `pinWeights`, `pinCorpus` — each pins its
  specific derivation

Each shell composes only the pins it actually uses (e.g. `triton`
pins weights, not corpus). On-demand data pins outside the shells:
`just fetch-regions` builds a regions DB and pins its root directly.
If a derivation that takes time to build is not pinned and not in the
store, it'll be silently re-fetched/rebuilt the next time the shell
loads.

## Lazy parameter passing

`flake.nix` has a soft cycle: `wikidata` needs `rust.commonArgs`, but
the test-running rust checks need `wikidata.bundles.curated.entities`
(via `testExtraEnv`). This works because Nix is lazy — `commonArgs` is
forced when computing `wikidata`, but `testExtraEnv` (and through it
the entities snapshot) is only forced when computing `checks.doctest`
/ `checks.llvm-cov` / `checks.web-test`. As long as `commonArgs`
itself doesn't reference `wikidata`, the cycle stays unresolved at the
right moment.

If you add similar test-time dependencies, follow the same pattern:
keep them out of `commonArgs`, route them through optional parameters
that are only consumed by check derivations.
