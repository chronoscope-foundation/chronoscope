# Model weights, fetched via `hf download` in sandboxed FODs.
#
# The export toolchain that consumes them lives in vision.nix; this module is
# only the fetch, so the one impure step in the chain stays in one place.
{ pkgs, lib }:

let
  # Only needs the HF CLI. The torch/transformers environment the exports run
  # in is built in vision.nix, against these outputs.
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

in
{
  inherit
    dinov3Repo
    sam3Repo
    sam3Cache
    ;
}
