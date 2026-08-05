# Python environments for analysis pipeline.
#
# Single environment: analysisEnv — full pipeline (torch, SAM3, DINOv3, lint, test)
# Model weights are fetched via `hf download` in sandboxed FODs and passed to
# check derivations and the dev shell via environment variables.
{ pkgs, lib }:

let
  # Python 3.13: binary cache coverage on aarch64-darwin (torch, torchvision, etc.).
  # SAM3 needs pkg_resources → setuptools < 81 (removed in 3.14).
  python = pkgs.python313;

  # SAM3 MPS/CPU fork — packaged from git.
  # Uses pkg_resources, so needs setuptools < 81.
  sam3 =
    assert lib.versionOlder python.pkgs.setuptools.version "81";
    python.pkgs.buildPythonPackage {
      pname = "sam3";
      version = "unstable-2025-01-01";
      pyproject = true;

      src = pkgs.fetchFromGitHub {
        owner = "Shreesh-Coder";
        repo = "sam3";
        rev = "447d702ffa5360802081d9dde076819b52b67e7d"; # feature/macos-cpu-mps
        hash = "sha256-jC/u4ZUenJcqO6UuU+wlZ/J/m0Ic6wqAiaeHb/NENdg=";
      };

      # Required: pyproject = true needs an explicit build system in nixpkgs.
      build-system = with python.pkgs; [
        setuptools
        wheel
      ];

      # Relax pinned versions: numpy<2 (works fine with 2.x), ftfy==6.1.1,
      # timm>=1.0.17 (1.0.15 in nixos-25.05 works fine)
      pythonRelaxDeps = [
        "numpy"
        "ftfy"
        "timm"
      ];

      dependencies = with python.pkgs; [
        torch
        torchvision
        numpy
        pillow
        einops
        pycocotools
        psutil
        setuptools # pkg_resources at runtime
        # Declared deps from pyproject.toml
        timm
        tqdm
        ftfy
        regex
        iopath
        huggingface-hub
        typing-extensions
      ];

      # SAM3 has no tests we can run in the sandbox
      doCheck = false;

      # Runs in installCheckPhase (not checkPhase), so works with doCheck = false.
      pythonImportsCheck = [ "sam3" ];

    };

  # Full analysis environment: runs SAM3 + DINOv3 pipeline, lint, and tests.
  analysisEnv = python.withPackages (ps: [
    # Core
    ps.numpy
    ps.pillow

    # ML models
    ps.torch
    ps.torchvision
    ps.transformers
    sam3
    ps.einops
    ps.pycocotools
    ps.psutil

    # Lint + type checking
    ps.ruff
    ps.mypy

    # Testing
    ps.pytest
    ps.hypothesis
    ps.jsonschema

    # setuptools for pkg_resources (SAM3 runtime dependency)
    ps.setuptools
  ]);

  # ---------------------------------------------------------------------------
  # Model weights — fetched via `hf download` in sandboxed FODs.
  # Gated models require HF_TOKEN for the first fetch (--impure). After the
  # output hash is known and in the Nix store, builds are pure.
  # Binary cache (Cachix) eliminates the token requirement for CI.
  # ---------------------------------------------------------------------------

  # FOD that downloads an HF model repo using `hf download` (the official
  # HuggingFace CLI). Handles Xet storage, LFS, and auth transparently.
  #
  # Auth: builtins.getEnv reads HF_TOKEN at eval time (requires --impure).
  # In pure mode it returns "" — fine, because if the output hash is already
  # known the derivation won't rebuild. First fetch of a gated model needs:
  #   HF_TOKEN=hf_... nix build --impure .#dinov3Repo
  # After that, the hash pins the result and pure builds work.
  hfEnv = python.withPackages (ps: [ ps.huggingface-hub ]);

  fetchHfRepo =
    {
      name,
      repo,
      rev,
      hash ? lib.fakeHash,
    }:
    pkgs.stdenvNoCC.mkDerivation {
      inherit name;

      outputHashMode = "recursive";
      outputHashAlgo = "sha256";
      outputHash = hash;

      nativeBuildInputs = [
        hfEnv
      ];

      # Token as a build-time env var via builtins.getEnv. For FODs the output
      # hash determines the store path, so different token values don't create
      # different outputs. Do NOT use impureEnvVars — the daemon overwrites
      # derivation env vars with its own (empty) values for listed vars.
      #
      # Caveat: the token is baked into the .drv file in the Nix store, which
      # is world-readable. On shared machines, `nix store delete` the .drv after
      # a successful fetch, or use a binary cache (Cachix) so the token is never
      # needed on the shared machine at all.
      # Read at eval time; "" in pure mode (harmless — output already cached).
      HF_TOKEN = builtins.getEnv "HF_TOKEN";

      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

      buildCommand = ''
        export HOME="$TMPDIR"
        if [ -z "$HF_TOKEN" ]; then
          echo "error: HF_TOKEN is not set." >&2
          echo "" >&2
          echo "  This is a gated model repo that requires a Hugging Face token." >&2
          echo "  Run: HF_TOKEN=hf_... just fetch-weights" >&2
          exit 1
        fi
        hf download "${repo}" \
          --revision "${rev}" \
          --local-dir "$out"
        # Remove download cache — contains store-path-dependent filenames
        # that would make the hash non-reproducible across derivation names.
        rm -rf "$out/.cache"
      '';
    };

  dinov3Repo = fetchHfRepo {
    name = "dinov3-vitl16-weights";
    repo = "facebook/dinov3-vitl16-pretrain-lvd1689m";
    rev = "ea8dc2863c51be0a264bab82070e3e8836b02d51";
    hash = "sha256-ooMiPFOEhZGarCDG27O+OxrO/V6OHkZ6SKwjdvGrfQk=";
  };

  # Shared between sam3Repo and sam3Cache (HF cache layout needs the rev hash).
  sam3WeightsRev = "3c879f39826c281e95690f02c7821c4de09afae7";

  sam3Repo = fetchHfRepo {
    name = "sam3-weights";
    repo = "facebook/sam3";
    rev = sam3WeightsRev;
    hash = "sha256-dKUMnUypPHmPy+RCryWFwqc2DwUHavpw97OZYZeFuT4=";
  };

  # SAM3 uses hf_hub_download internally, which expects the HF cache layout:
  #   $HF_HOME/hub/models--{org}--{repo}/refs/main     (text: commit hash)
  #   $HF_HOME/hub/models--{org}--{repo}/snapshots/{rev}/  (repo contents)
  #
  # We construct a minimal cache pointing at the fetchHfRepo result.
  sam3Cache = pkgs.runCommand "sam3-hf-cache" { } ''
    model_dir="$out/hub/models--facebook--sam3"
    snapshot_dir="$model_dir/snapshots/${sam3WeightsRev}"
    mkdir -p "$model_dir/refs" "$snapshot_dir"
    echo -n "${sam3WeightsRev}" > "$model_dir/refs/main"
    # Symlink all files (including dotfiles like .gitattributes) into the snapshot
    shopt -s dotglob nullglob
    for f in ${sam3Repo}/*; do
      ln -s "$f" "$snapshot_dir/$(basename "$f")"
    done
  '';

  # Two source sets from analysis/triton/:
  #
  #   tritonSrc   — everything: models, runner, tests, config.
  #                 Used by Python checks (triton-test, triton-lint, etc.).
  #
  #   analysisSrc — models and runner only, no tests.
  #                 Used by analysisResults (expensive GPU derivation) and
  #                 any future Nix-built container image. Editing a test
  #                 file won't trigger a multi-hour GPU rebuild.
  #
  # analysisSrc is a further filter on tritonSrc, so the cache-exclusion
  # logic (mypy, pycache, etc.) is shared.

  tritonSrc = lib.cleanSourceWith {
    src = ../analysis/triton;
    filter =
      path: type:
      let
        baseName = builtins.baseNameOf path;
      in
      # Exclude dev caches that poison the store hash from dirty worktrees.
      baseName != ".mypy_cache"
      && baseName != "__pycache__"
      && baseName != ".hypothesis"
      && baseName != ".pytest_cache"
      && baseName != ".ruff_cache"
      && (
        (lib.hasSuffix ".py" path)
        || (lib.hasSuffix ".json" path)
        || (lib.hasSuffix ".pbtxt" path)
        || (lib.hasSuffix ".toml" path)
        || (type == "directory")
      );
    name = "triton-source";
  };

  analysisSrc = lib.cleanSourceWith {
    src = tritonSrc;
    filter =
      path: _type:
      let
        baseName = builtins.baseNameOf path;
      in
      baseName != "test_models.py" && baseName != "conftest.py";
    name = "analysis-source";
  };

  # Shared model inference env vars — used by analysisResults, Python checks,
  # and the dev shell. Each site may layer additional vars (e.g. SSL_CERT_FILE).
  modelEnv = {
    DINOV3_MODEL_DIR = dinov3Repo;
    HF_HOME = sam3Cache;
    HF_HUB_OFFLINE = "1";
    PYTORCH_ENABLE_MPS_FALLBACK = "1";
  };

in
{
  inherit
    analysisEnv
    dinov3Repo
    sam3Cache
    modelEnv
    tritonSrc
    analysisSrc
    ;

  # Python check derivations for nix flake check.
  #
  # Lints only. The pytest suite ran here until it was dropped: it needed
  # `schematool` on PATH for cross-language schema validation, and took it from
  # `rust.packages.default`, so every gate compiled the whole workspace in the
  # default profile to obtain one binary. That build is 5m27s and the largest
  # single item in the gate, and with 18 concurrent jobs against 18 cores it
  # slowed every check running beside it: excluding this one check took a gate
  # from 601s to 325s.
  #
  # Restoring the suite means building `schematool` on its own, the way `ingest`
  # already is — `deployables` in `flake.nix` plus `mkWorkspaceSrc` exist for
  # exactly that, and `workspace-closures` would then hold its crate list to
  # what cargo reports. Worth doing with whatever replaces this harness rather
  # than to prop up a harness on its way out.
  checks =
    let
      # Lightweight check: no model weights, no rust binary.
      mkLintCheck =
        name: script:
        pkgs.runCommand "triton-${name}"
          {
            nativeBuildInputs = [ analysisEnv ];
            src = tritonSrc;
          }
          ''
            cp -r $src src
            chmod -R u+w src
            cd src
            ${script}
            touch $out
          '';

    in
    {
      triton-fmt = mkLintCheck "fmt" ''
        ruff format --check .
      '';

      triton-lint = mkLintCheck "lint" ''
        ruff check .
      '';

      triton-typecheck = mkLintCheck "typecheck" ''
        # Check test/mock files together
        mypy --ignore-missing-imports mock_triton.py conftest.py test_models.py
        # Check each model file separately (Triton requires model.py naming)
        for f in models/*/1/model.py models/*/1/baml_converter.py; do
          if [ -f "$f" ]; then
            mypy --ignore-missing-imports "$f"
          fi
        done
      '';
    };
}
