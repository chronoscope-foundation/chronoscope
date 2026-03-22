{
  description = "Chronoscope — temporal and spatial analysis platform";

  inputs = {
    # Pinned to a nixpkgs-unstable commit with:
    #   torch 2.10.0 (cached on aarch64-darwin; 2.9.x has MPS torch.cat crash)
    #   transformers 5.2.0 (>= 4.56.0 required for dinov3_vit model type)
    #   huggingface-hub 1.4.1 (has `hf` CLI for model downloads)
    #   setuptools 80.10.1 (< 81; SAM3 needs pkg_resources)
    nixpkgs.url = "github:NixOS/nixpkgs/d5a3c4d6c0b82a89b9b6a32a4d6036e762fbca3f";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      crane,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;

        # Rust toolchain from fenix stable channel.
        # fenix pin (via flake.lock) determines the exact stable version.
        toolchain =
          with fenix.packages.${system};
          combine [
            stable.cargo
            stable.clippy
            stable.rustc
            stable.rustfmt
            stable.rust-src
            stable.llvm-tools-preview
            targets.wasm32-unknown-unknown.stable.rust-std
          ];

        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        # Source filtering: Cargo sources + .proto (protobuf) + .sql (migrations)
        # + .json (test fixtures, schemas) + .html/.css (web frontend).
        src = lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter =
            path: type:
            (craneLib.filterCargoSources path type)
            || (lib.hasSuffix ".proto" path)
            || (lib.hasSuffix ".sql" path)
            || (lib.hasSuffix ".json" path)
            || (lib.hasSuffix ".html" path)
            || (lib.hasSuffix ".css" path);
          name = "chronoscope-source";
        };

        # Phase 1: Rust workspace builds and checks.
        rust = import ./nix/rust.nix {
          inherit
            pkgs
            craneLib
            lib
            src
            ;
        };

        # Phase 2: Python environments and corpus pipeline.
        pythonEnvs = import ./nix/python.nix { inherit pkgs lib; };

        corpus = import ./nix/corpus.nix {
          inherit
            pkgs
            lib
            craneLib
            ;
          inherit pythonEnvs;
          rustCommonArgs = rust.commonArgs;
        };

        # Phase 3: Web frontend (WASM).
        web = import ./nix/web.nix {
          inherit
            pkgs
            lib
            fenix
            crane
            system
            src
            ;
        };

        pythonChecks = pythonEnvs.checks {
          rustPackage = rust.packages.default;
        };

        # Nix source for lint checks (only .nix files, excludes .git/).
        nixSrc = lib.cleanSourceWith {
          # cleanSource strips .git/ (which would pass the type == "directory" filter below).
          src = lib.cleanSource ./.;
          filter = path: type: (type == "directory") || (lib.hasSuffix ".nix" path);
          name = "nix-source";
        };
      in
      {
        # `nix flake check` — all quality gates.
        checks =
          rust.checks
          // web.checks
          // pythonChecks
          // {
            nix-lint =
              pkgs.runCommand "nix-lint"
                {
                  nativeBuildInputs = [
                    pkgs.nixfmt
                    pkgs.statix
                    pkgs.deadnix
                  ];
                  src = nixSrc;
                }
                ''
                  cd $src
                  find . -name '*.nix' -print0 | xargs -0 nixfmt --check
                  statix check .
                  find . -name '*.nix' -print0 | xargs -0 deadnix --fail -L
                  touch $out
                '';
            # corpus-tests intentionally excluded — requires GPU (run on
            # dedicated CI runners via `nix build .#corpus-tests`).
          };

        # `nix build` — workspace binaries.
        # analysis-results requires torch (available on all eachDefaultSystem platforms
        # in nixpkgs, but only with CUDA on x86_64-linux). Guard with a comment so
        # future platform additions consider torch availability.
        packages =
          rust.packages
          // web.packages
          // {
            corpus-images = corpus.corpusImages;
            corpus-fetch = corpus.corpusFetchBin;
            corpus-tests = corpus.corpusTests;
            analysis-results = corpus.analysisResults;
            # Model weights — build with --impure and HF_TOKEN to populate store.
            dinov3-weights = pythonEnvs.dinov3Repo;
            sam3-weights = pythonEnvs.sam3Cache;
          };

        # `nix fmt` — format Nix files.
        formatter = pkgs.nixfmt;

        # `nix develop` — three-tier interactive development shells.
        #
        # default:  Rust + Python + lint tools. No model weights or corpus images.
        #           Good for web dev, API work, and most of the repo.
        #
        # analysis: default + model weights (DINOv3, SAM3). For running
        #           analysis pipeline tests. Requires `just fetch-weights` first.
        #
        # corpus:   analysis + corpus images. For the full corpus test suite.
        #           Requires `just fetch-corpus` (or `just fetch-all`) first.
        #
        # Model weights and corpus images are fetched on demand via just recipes
        # rather than as Nix derivation dependencies, because they require network
        # access to gated HF repos (which require an HF account and token) and
        # external URLs that can be rate-limited.

        devShells =
          let
            # Shared inputs and env across all shell tiers.
            baseNativeBuildInputs = rust.devShell.nativeBuildInputs ++ [
              toolchain
              pythonEnvs.analysisEnv
              pkgs.nixfmt
              pkgs.statix
              pkgs.deadnix
            ];

            baseEnv = rust.devShell.env // {
              RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
              CORPUS_MANIFEST = corpus.corpusManifestJson;
              PYTORCH_ENABLE_MPS_FALLBACK = "1";
              HF_HUB_OFFLINE = "1";
            };

            # Pin store paths as GC roots so determinate-nixd auto-GC won't collect them.
            gcRootPreamble = ''
              _gc_root_dir="$(git rev-parse --show-toplevel 2>/dev/null || echo .)/.nix-gc-roots"
              mkdir -p "$_gc_root_dir"
            '';

            pinWeightsAsRoots = ''
              nix-store --realise ${pythonEnvs.dinov3Repo} --add-root "$_gc_root_dir/dinov3-weights" > /dev/null 2>&1
              nix-store --realise ${pythonEnvs.sam3Cache} --add-root "$_gc_root_dir/sam3-weights" > /dev/null 2>&1
            '';

            pinCorpusAsRoots = ''
              nix-store --realise ${corpus.corpusImages} --add-root "$_gc_root_dir/corpus-images" > /dev/null 2>&1
            '';

            shellInfo = ''
              echo "chronoscope dev shell"
              echo "  rust: $(rustc --version)"
              echo "  protoc: $($PROTOC --version)"
              echo "  python: $(python3 --version)"
            '';
          in
          {
            default = pkgs.mkShell {
              nativeBuildInputs = baseNativeBuildInputs;
              inherit (rust.devShell) buildInputs;
              env = baseEnv;
              shellHook = ''
                ${shellInfo}
              '';
            };

            analysis = pkgs.mkShell {
              nativeBuildInputs = baseNativeBuildInputs;
              inherit (rust.devShell) buildInputs;
              env =
                baseEnv
                // pythonEnvs.modelEnv;
              shellHook = ''
                ${gcRootPreamble}
                ${pinWeightsAsRoots}
                ${shellInfo}
              '';
            };

            corpus = pkgs.mkShell {
              nativeBuildInputs = baseNativeBuildInputs;
              inherit (rust.devShell) buildInputs;
              env =
                baseEnv
                // pythonEnvs.modelEnv
                // {
                  CORPUS_IMAGES = corpus.corpusImages;
                };
              shellHook = ''
                ${gcRootPreamble}
                ${pinWeightsAsRoots}
                ${pinCorpusAsRoots}
                ${shellInfo}
              '';
            };
          };
      }
    );
}
