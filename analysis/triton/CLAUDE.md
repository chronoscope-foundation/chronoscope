# analysis/triton

Python side of the analysis pipeline — Triton serving harness, SAM3 +
DINOv3 model wrappers, cross-language schema validation. Loaded
automatically when working under `analysis/triton/`. The parent
`analysis/CLAUDE.md` also applies.

## Cross-language schema validation

Python tests shell out to `schematool` (a Rust binary at
`analysis/src/bin/schematool.rs`) to confirm Python model output matches
the Rust-side schema. This is the only reason these Python tests need
cargo built first.

- In `nix flake check`: `schematool` comes prebuilt from
  `rust.packages.default` (passed in via the `mkModelCheck` helper in
  `nix/python.nix`).
- In `devShells.triton`: same — `rust.packages.default` is in
  `nativeBuildInputs`, so `schematool` is on PATH.

If a triton test starts failing with "schema mismatch", the most likely
cause is a Rust-side schema change without a corresponding Python-side
change (or vice versa). The test catches that drift on purpose.

## SAM3's HF cache layout (the part that bites you once)

SAM3 uses `hf_hub_download` internally, which expects a specific HF
cache layout under `$HF_HOME`:

```
$HF_HOME/hub/models--facebook--sam3/refs/main         ← text: commit hash
$HF_HOME/hub/models--facebook--sam3/snapshots/{rev}/  ← repo contents
```

`nix/python.nix`'s `sam3Cache` derivation constructs this layout from
the FOD'd weights (`sam3Repo`). If you edit the SAM3 integration and
get baffling "model not found" errors despite weights clearly being
present, **check the cache layout first** — the model files are in the
store but not at the path SAM3 expects.

## HF_HUB_OFFLINE=1

All shells with weights set `HF_HUB_OFFLINE=1`, which means missing
weights fail hard rather than silently downloading. If you see a
"weights not found" error, run `HF_TOKEN=hf_... just fetch-weights`
to populate the Nix store. After the first fetch, the hash is pinned
and pure builds work without the token.

## Source filtering for cache stability

Two source sets are exposed by `nix/python.nix`:

- `tritonSrc` — full directory (models + tests + config). Used by lint
  and test checks.
- `analysisSrc` — strict subset, **excluding** `test_models.py` and
  `conftest.py`. Used by the expensive `analysisResults` GPU
  derivation, so editing a test file doesn't trigger a multi-hour GPU
  rebuild.

Both filter dev caches (`.mypy_cache`, `__pycache__`, etc.) so dirty
worktrees don't poison the store hash.
