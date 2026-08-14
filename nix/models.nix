# Model weights, fetched via `hf download` in sandboxed FODs — the one impure
# step of the build chain, kept in a single module.
#
# DINOv3 and SAM 3 weights feed the ONNX export toolchain in analysis.nix; the
# Qwen VLM weights are loaded straight by the analysis crate's mistral.rs runtime
# with no export step. Python appears below only as the vehicle for the `hf` CLI.
{ pkgs, lib }:

let
  # Only needs the HF CLI. The torch/transformers environment the exports run
  # in is built in analysis.nix, against these outputs.
  python = pkgs.python313;
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
      # Gated repos hard-fail without a token; ungated ones (the Qwen VLM is
      # apache-2.0) download fine without one, so the check would only lie.
      gated ? true,
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
        ${lib.optionalString gated ''
          if [ -z "$HF_TOKEN" ]; then
            echo "error: HF_TOKEN is not set." >&2
            echo "" >&2
            echo "  This is a gated model repo that requires a Hugging Face token." >&2
            echo "  Run: HF_TOKEN=hf_... just fetch-weights" >&2
            exit 1
          fi
        ''}
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

  # The Qwen vision-language MoE mistral.rs runs as the VLM: the base BF16
  # safetensors repo (~67 GiB, 26 shards), which mistral.rs ISQ-quantizes at
  # load. Nothing smaller substitutes — this version ships no UQFF, and
  # mistral.rs cannot load GGUF for the qwen3_5_moe arch (upstream issues #2049
  # text, #1714 vision). The base repo also carries the vision tower and loads
  # from config.json's `Qwen3_5MoeForConditionalGeneration` with no override.
  # Ungated (apache-2.0): no HF_TOKEN.
  #
  # Version is the swap knob: 3.6 → 3.8 is the string below, plus its matching
  # rev and content hash — both identify the model, so both move with it.
  qwenVlmVersion = "3.6";

  qwenVlm = fetchHfRepo {
    name = "qwen${qwenVlmVersion}-35b-a3b-vlm-weights";
    repo = "Qwen/Qwen${qwenVlmVersion}-35B-A3B";
    rev = "995ad96eacd98c81ed38be0c5b274b04031597b0";
    gated = false;
    hash = "sha256-I2OTeT7X+HA124JAXWL1gGbHoT4ng3SSYIDV6v0RzEI=";
  };

in
{
  inherit
    dinov3Repo
    sam3Repo
    sam3Cache
    qwenVlm
    ;
}
