# nix/

Nix build infrastructure. One derivation module per project area;
`flake.nix` composes them.

## Layout

| Module          | What it owns                                        |
|-----------------|-----------------------------------------------------|
| `rust.nix`      | Workspace `commonArgs`, native checks, `default` package |
| `api.nix`       | Wrapped `chronoscope-api` binary (SpatiaLite path baked in) |
| `openapi.nix`   | OpenAPI `spec`, ChronoscopeAPI SwiftPM package, store-path-spliced xcodegen `projectSpec` |
| `web.nix`       | WASM build pipeline + wasm `web-build`/`web-test-build`/`web-clippy` and host `web-native-test`/`web-native-clippy` checks |
| `python.nix`    | Model weight FODs (the one impure step in the chain) |
| `analysis.nix`  | Patched sam3/samexporter packages, the ONNX export derivations and their manifests, and the per-resolution reference fixtures |
| `corpus.nix`    | Per-URL image FODs, the per-entry `imageFiles` map that selects one of them, the link farm they assemble into, and the `corpus-fetch` binary that hashes new URLs |
| `wikidata.nix`  | Curated entity fetch FOD + bulk dump pipeline (aria2 torrent FOD → arch-types → arch-entities JSONL → SQLite facts DBs) |
| `oci.nix`       | nix2container image for the API server (Cloud Run) + the check that boots its entrypoint, Linux systems only |
| `workspace-src.nix` | Narrowed sources for artifacts built from one workspace package, plus `workspace-closures`, the check that keeps the declared crate lists matching cargo |
| `infra.nix`     | terranix modules for the cloud project, the `config.tf.json` they compile to, the pinned OpenTofu, and the check that validates the two together |
| `infra-settings.nix` | The project's cloud coordinates (project, region, registry, state bucket, Cloudflare account/zone). Read by `infra.nix` and by the justfile via `nix eval --file`, so there is one definition |
| `front-door.js` | The Cloudflare Worker `infra.nix` reads inline: serves the web bundle as static assets, proxies `/api` to Cloud Run with the mount stripped |

Dev shell composition lives in `flake.nix`, not in any single component
module — it has the visibility to compose across modules.

## Infrastructure modules

`infra.nix` is one terranix module per provider (`gcp`, `cloudflare`),
merged in `modules`. Provider versions come from the nixpkgs derivation
in both places they appear (`withPlugins` and `required_providers`), so
the tofu the shell runs and the constraint the config carries cannot
drift.

Each provider's artifact arrives as a variable rather than being baked
into the config: `var.image` for Cloud Run, `var.web_dist` for the
Worker's static assets. `var.web_dist` differs in one way that matters —
the Cloudflare provider **reads that directory while planning** (it
hashes every file to decide what to upload), so it must be a path that
exists right now. That is why the justfile builds `packages.web` on
every plan and apply instead of carrying the last value forward out of
the state the way it does for the image.

`checks.infra-validate` runs `tofu validate` with no credentials and no
network, so a resource argument that does not exist fails at commit time
rather than partway through an apply. Adding a provider means adding it
to `withPlugins`, or `init` inside that check cannot resolve it.

## Deployable sources

A build that names one package compiles only that package's closure, but a
workspace has one source tree. Handing the whole tree to such a build puts
every other crate in its input hash, so an unrelated edit yields a different
output path: a new container image, a republished bundle, a rebuilt pipeline
that cannot have changed.

`deployables` in `flake.nix` registers each such artifact with the crates it
compiles and the non-cargo files its build reads (embedded `.sql` migrations,
`.proto` a build script compiles, the stylesheet a bundler consumes).
`mkWorkspaceSrc` turns that into a source: every member's `Cargo.toml`, since
cargo will not load a workspace whose members it cannot read, plus the full
contents of the listed crates.

The lists are allow-lists on purpose. A crate missing from one has no sources
and the build stops; an unrelated crate missing from a deny-list would quietly
rejoin the hash, and the churn would return with nothing to signal it.

**Adding a container means adding an entry**, not writing a fileset. The
`workspace-closures` check then covers it: it reads the real graph from
`cargo metadata --no-deps` and fails naming the crate to add or remove, which
matters because cargo's own response to a wrong list is a warning printed while
it drops the dependency.

## The function-with-named-instances pattern

Where a derivation has variants that are interesting at the flake level
(not just internal), expose:

1. A function that takes the variant input (e.g. `mkBundle name bundle`).
2. Named instances in `flake.nix`'s `packages` (e.g.
   `wikidata-curated-entities`).

Adding a new variant is one entry in `flake.nix` — no new branches in
the function itself.

## Coverage rule for `just check`

`just check` is hermetic and equivalent to `nix flake check` (or a
focused subset). **Every gate the project commits behind must be a
flake check.** If you add a new check (e.g. a new lint, a new build),
expose it via `checks.<system>.<name>`, not just as a step in the
justfile — otherwise it slips past the pre-commit gate.

A check behind `lib.optionalAttrs isLinux` is a flake check that the gate
still cannot see, since only the current system is evaluated. Those are
named explicitly by `just check linux`, which runs outside the commit gate
(see the root CLAUDE.md for when to reach for it).

## GC root pinning

Large derivations (model weights, corpus images, the wikidata entities
snapshot) get pinned as GC roots in `.nix-gc-roots/`
(gitignored) so they survive `nix store gc`. Pinning happens in shell
hooks:

- `gcRootsPrelude` — creates `.nix-gc-roots/`
- `pinWikidataRoot`, `pinCorpus` — each pins its specific derivation

Each shell composes only the pins it actually uses (`api`, `web` and
`analysis` all pin the wikidata snapshot; only `analysis` pins the
corpus images). If a derivation that takes time to build is not
pinned and not in the store, it'll be silently re-fetched/rebuilt the
next time the shell loads.

A shell hook can only pin what it can afford to realize on `cd`, which
leaves out the two heaviest fetches. Building `wikidata-arch-entities`
downloads the ~109GB dump and runs the filter; the model weights are
gated repos that need an `HF_TOKEN` and several GB. Both are pinned by
the recipe that fetches them — `just fetch-wikidata`, `just
fetch-weights` — via `--out-link`, which is itself a GC root. They live
in `packages`, not `checks`, so the commit gate never touches them.

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
