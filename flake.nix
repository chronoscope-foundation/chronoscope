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
          ];

        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        # Source filtering: Cargo sources + .proto (protobuf) + .sql (migrations)
        # + .json (test fixtures, schemas).
        src = lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter =
            path: type:
            (craneLib.filterCargoSources path type)
            || (lib.hasSuffix ".proto" path)
            || (lib.hasSuffix ".sql" path)
            || (lib.hasSuffix ".json" path);
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
        packages = rust.packages // {
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

        # `nix develop` — interactive development shell.
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = rust.devShell.nativeBuildInputs ++ [
            toolchain
            pythonEnvs.analysisEnv
            pkgs.nixfmt
            pkgs.statix
            pkgs.deadnix
          ];

          inherit (rust.devShell) buildInputs;

          env =
            rust.devShell.env
            // pythonEnvs.modelEnv
            // {
              # fenix's combined toolchain has a sysroot layout rust-analyzer
              # can't always auto-detect. Explicit path is the standard Nix workaround.
              RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
              # Corpus manifest for Rust (Nix-evaluated JSON of analysis/corpus.nix)
              CORPUS_MANIFEST = corpus.corpusManifestJson;
              # Pre-fetched corpus images (Nix link farm: entry-id → image file)
              CORPUS_IMAGES = corpus.corpusImages;
              # ANALYSIS_RESULTS is NOT set here — it requires the expensive GPU
              # derivation, which would block `nix develop`. Corpus test recipes
              # build it on demand via `nix build .#analysis-results`.
            };

          shellHook = ''
            # Pin large store paths as GC roots so determinate-nixd auto-GC won't collect them.
            mkdir -p .nix-gc-roots
            nix-store --realise ${pythonEnvs.dinov3Repo} --add-root .nix-gc-roots/dinov3-weights > /dev/null 2>&1 || true
            nix-store --realise ${pythonEnvs.sam3Cache} --add-root .nix-gc-roots/sam3-weights > /dev/null 2>&1 || true
            nix-store --realise ${corpus.corpusImages} --add-root .nix-gc-roots/corpus-images > /dev/null 2>&1 || true

            echo "chronoscope dev shell"
            echo "  rust: $(rustc --version)"
            echo "  protoc: $(protoc --version)"
            echo "  python: $(python3 --version)"
          '';
        };
      }
    );
}
