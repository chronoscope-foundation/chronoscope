# chronoscope-analysis

Image analysis pipeline (SAM3 + DINOv3) plus the Rust-side corpus test
infrastructure. Loaded automatically when working anywhere under `analysis/`.
(The Python triton-side has its own additional CLAUDE.md at
`analysis/triton/CLAUDE.md`.)

## Adding a corpus image (the 3-step ritual)

1. Edit `analysis/corpus.nix` — add an entry to `images` (and optionally
   to a cluster). For Reddit galleries, set `reddit_index` to the
   specific file you want.
2. `just corpus-hash` — runs the corpus-fetch binary in `hash` mode to
   add the new URL's SRI hash to `analysis/corpus-hashes.json`. (Reddit
   galleries are resolved inside the sandboxed FOD; ephemeral CDN URLs
   don't leak into the hash.)
3. Next run of `just corpus-test` rebuilds `analysis-results` (the GPU
   derivation) and re-runs the Rust corpus tests against the new JSONL.

## Corpus FOD layout

Per-URL FODs in a link farm. Defined in `nix/corpus.nix`:

- Each unique URL → one fixed-output derivation.
- Reddit galleries: multi-file FOD, files indexed by `reddit_index`
  (0, 1, 2, ...).
- Direct URLs: always index 0.
- Link farm maps entry ID → specific file in the FOD: `${fod}/${index}`.

The link farm is what `find -L` walks during analysis to feed entry IDs
(as filenames) into `run_local.py`.

## Build-time vs read-time mental model

GPU work happens **at Nix build time** (the `analysis-results` derivation
runs SAM3 + DINOv3 over the corpus, producing JSONL). Rust corpus tests
**read the JSONL at test time**. This is why `just corpus-test` is fast
once `analysis-results` is cached — the GPU work is amortized.

If you change a model config or analysis source, `analysis-results`
invalidates and the next test run rebuilds it (slow once, fast after).

## Feature flags

- `corpus-test` — gates the `corpus_tests` integration test target.
  Default builds don't compile it (no manifest reads). Required for
  `just corpus-test`, `just corpus-hash`, and `just clippy analysis` if
  you want corpus-side code linted.
- `corpus-test-vlm` — same but adds VLM (Vision-Language Model) tests.
  Requires a remote Triton server and is gated to `just corpus-test-vlm`.

The hermetic `nix flake check` runs corpus-tests via `nix/corpus.nix`'s
`corpusTests` derivation, **not** through the workspace test check
(which would need GPU access during `nix flake check`). To run them
locally: `just corpus-test`.
