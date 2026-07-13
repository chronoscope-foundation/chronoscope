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
        inherit (pkgs.stdenv.hostPlatform) isDarwin;

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

        rust = import ./nix/rust.nix {
          inherit
            pkgs
            craneLib
            lib
            src
            ;
          # Lazy: only `test`/`llvm-cov` force these, so the wikidata/web
          # cycle stays unresolved at eval time.
          testExtraEnv = apiRuntimeEnv // webEnv;
        };

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

        wikidata = import ./nix/wikidata.nix {
          inherit
            pkgs
            lib
            craneLib
            ;
          rustCommonArgs = rust.commonArgs;
          inherit (rust) cargoArtifacts;
        };

        web = import ./nix/web.nix {
          inherit
            pkgs
            lib
            fenix
            crane
            craneLib
            system
            src
            ;
          # Native (non-wasm) crane bits for the host-target `web-native-test`
          # check — the wasm pipeline can't run the crate's plain #[test]s.
          rustCommonArgs = rust.commonArgs;
        };

        api = import ./nix/api.nix {
          inherit pkgs craneLib;
          rustCommonArgs = rust.commonArgs;
          inherit (rust) cargoArtifacts;
        };

        # Swift sources for the ChronoscopeAPI package, filtered so a source
        # edit is the only thing that rebuilds the store package.
        iosApiSrc = lib.cleanSourceWith {
          src = ./ios/ChronoscopeAPI;
          filter =
            path: type:
            (type == "directory")
            || lib.hasSuffix ".swift" path
            || lib.hasSuffix ".yaml" path
            || lib.hasSuffix ".yml" path;
          name = "chronoscope-ios-api-src";
        };

        openapi = import ./nix/openapi.nix {
          inherit pkgs craneLib iosApiSrc;
          iosProjectTemplate = ./ios/project.yml;
          rustCommonArgs = rust.commonArgs;
          inherit (rust) cargoArtifacts;
        };

        pythonChecks = pythonEnvs.checks {
          rustPackage = rust.packages.default;
        };

        # Nix source for lint check (excludes .git/).
        nixSrc = lib.cleanSourceWith {
          src = lib.cleanSource ./.;
          filter = path: type: (type == "directory") || (lib.hasSuffix ".nix" path);
          name = "nix-source";
        };

        # webauthn-rs links openssl-sys unconditionally; libspatialite is
        # loaded at runtime via SELECT load_extension.
        backendNativeBuildInputs = with pkgs; [ pkg-config ];
        backendBuildInputs =
          (with pkgs; [
            sqlite
            openssl
            libspatialite
          ])
          ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
        # Runtime env for the api/db/ingestion code paths (consumed by the
        # backend/web dev shells AND by the hermetic test/llvm-cov checks).
        apiRuntimeEnv = {
          SPATIALITE_LIBRARY_PATH = "${pkgs.libspatialite}/lib";
          WIKIDATA_ENTITIES_JSONL = "${wikidata.bundles.curated.entities}/entities.jsonl";
        };
        backendEnv = apiRuntimeEnv // {
          PROTOC = "${pkgs.protobuf}/bin/protoc";
        };

        # WASM frontend tooling (trunk, wasm-bindgen, tailwind).
        webNativeBuildInputs = with pkgs; [
          binaryen
          tailwindcss_4
          trunk
          wasm-bindgen-cli
        ];

        # Full Chromium (not chrome-headless-shell): headless-shell only ships
        # SwiftShader software WebGL, which contends in parallel maplibre tests.
        # Wrapper is a script that exec's the absolute path — a symlink would
        # break macOS chrome's data-file lookup (icudtl.dat), since
        # _NSGetExecutablePath returns the symlink target dir, not the bundle.
        chromeHeadless = pkgs.runCommand "chromium-wrapper" { } ''
          mkdir -p $out/bin
          # Exclude chromium_* (headless-shell sibling) — playwright has
          # been inconsistent on the separator, so don't trust the dash alone.
          chromium_root=$(find -L ${pkgs.playwright-driver.browsers} \
            -maxdepth 1 -type d -name 'chromium-*' ! -name 'chromium_*' | head -n1)
          if [ -z "$chromium_root" ]; then
            echo "error: chromium- root not found inside playwright-driver.browsers" >&2
            exit 1
          fi
          src=$(find -L "$chromium_root" \
            \( -name 'Google Chrome for Testing' -o -name chrome \) \
            -type f -perm -u+x | head -n1)
          if [ -z "$src" ]; then
            echo "error: chromium binary not found inside $chromium_root" >&2
            exit 1
          fi
          cat > $out/bin/chromium <<EOF
          #!/bin/sh
          exec "$src" "\$@"
          EOF
          chmod +x $out/bin/chromium
        '';
        chromeHeadlessBin = "${chromeHeadless}/bin/chromium";

        webEnv = {
          WEB_DIST = web.packages.web-test;
          # chromiumoxide picks up CHROME as the executable path.
          CHROME = chromeHeadlessBin;
          # The read-only facts DB the dev servers and browser tests mount (a
          # per-run CoW clone; see chronoscope-dev's mount_facts_db). Lives in
          # webEnv, not apiRuntimeEnv: it drags the ingest binary build into
          # its closure, which the web shell already pays for WEB_DIST but the
          # lighter shells must not.
          CHRONOSCOPE_FACTS_DB = "${wikidata.factsDbs.curated}/facts.db";
        };

        # Tools every shell wants on PATH.
        commonTools = with pkgs; [
          just
          nixfmt
          statix
          deadnix
          cargo-llvm-cov
        ];

        commonEnv = {
          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          PYTORCH_ENABLE_MPS_FALLBACK = "1";
          HF_HUB_OFFLINE = "1";
        };

        # Pin commonly-resolved derivations as GC roots so they survive
        # store collection. Each shell pins what it materially uses.
        # Skips the daemon roundtrip when the symlink already points at the
        # current store path (the eval pinned the path, so a match means the
        # root is current).
        gcRootsPrelude = ''
          _gc_root_dir="$(git rev-parse --show-toplevel 2>/dev/null || echo .)/.nix-gc-roots"
          mkdir -p "$_gc_root_dir"
          _pin() {
            if [ "$(readlink "$_gc_root_dir/$1" 2>/dev/null)" != "$2" ]; then
              nix-store --realise "$2" --add-root "$_gc_root_dir/$1" > /dev/null 2>&1
            fi
          }
        '';
        pinWikidataRoot = "_pin wikidata-entities ${wikidata.bundles.curated.entities}";
        pinWeights = ''
          _pin dinov3-weights ${pythonEnvs.dinov3Repo}
          _pin sam3-weights ${pythonEnvs.sam3Cache}
        '';
        pinCorpus = "_pin corpus-images ${corpus.corpusImages}";

        mkBanner =
          name: extras:
          ''
            echo "chronoscope dev shell (${name})"
            echo "  rust: $(rustc --version 2>/dev/null || echo 'not in this shell')"
          ''
          + extras;
      in
      {
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

            openapi = openapi.spec;

            # Hermetic coverage of the build-db path: a dump-free facts DB from
            # the curated bundle, self-validating that it holds facts.
            wikidata-facts-db-curated = wikidata.factsDbs.curated;
            # corpus-tests intentionally excluded — requires GPU (run on
            # dedicated CI runners via `nix build .#corpus-tests`).
          }
          // lib.optionalAttrs isDarwin {
            ios-project-spec = openapi.projectSpec;
          };

        packages =
          rust.packages
          // web.packages
          // {
            inherit (api) api;
            openapi = openapi.spec;

            corpus-images = corpus.corpusImages;
            corpus-fetch = corpus.corpusFetchBin;
            corpus-tests = corpus.corpusTests;
            analysis-results = corpus.analysisResults;

            # Model weights — built with --impure and HF_TOKEN to populate store.
            dinov3-weights = pythonEnvs.dinov3Repo;
            sam3-weights = pythonEnvs.sam3Cache;

            wikidata-curated-entities = wikidata.bundles.curated.entities;

            # Bulk dump pipeline. Packages, not checks — the commit gate
            # never downloads the dump. Build + pin the architectural-entities
            # set with `just fetch-wikidata`.
            # Core-free dump-filter tool (fetch/resolve-types/filter), built
            # from a narrowed source so core/db churn doesn't rebuild it.
            wikidata-dump-tool = wikidata.dumpToolBin;

            wikidata-dump = wikidata.dump.full;
            wikidata-arch-types = wikidata.dump.archTypes;
            wikidata-arch-entities = wikidata.dump.archEntities;

            # SQLite fact-store DBs built from the entities via `ingest
            # build-db`. `curated` is dump-free; the sizes slice first-N.
            wikidata-facts-db-curated = wikidata.factsDbs.curated;
            wikidata-facts-db-1k = wikidata.factsDbs."1k";
            wikidata-facts-db-100k = wikidata.factsDbs."100k";
            wikidata-facts-db-full = wikidata.factsDbs.full;
          }
          // lib.optionalAttrs isDarwin {
            ios-api-package = openapi.apiPackage;
            ios-project-spec = openapi.projectSpec;
          };

        formatter = pkgs.nixfmt;

        devShells = {
          # Minimum to enter the project: rust toolchain + just + nix lint
          # tools. No backend native deps, no wasm tooling, no model weights.
          # Most cargo invocations from here will fail to link — switch into
          # a component shell, or use `just <recipe>` which re-execs.
          default = pkgs.mkShell {
            nativeBuildInputs = [ toolchain ] ++ commonTools;
            env = commonEnv;
            shellHook = ''
              ${gcRootsPrelude}
              ${mkBanner "default — minimal" ""}
            '';
          };

          # Backend / API server work: full native deps for the workspace
          # default-members. Default for crates that don't have a more
          # specific shell (core, db, ingestion, workers, dev, api-client).
          api = pkgs.mkShell {
            nativeBuildInputs = [ toolchain ] ++ commonTools ++ backendNativeBuildInputs;
            buildInputs = backendBuildInputs;
            env = commonEnv // backendEnv;
            shellHook = ''
              ${gcRootsPrelude}
              ${pinWikidataRoot}
              ${mkBanner "api" ""}
            '';
          };

          # Web frontend work: backend stack (so `cargo run -p chronoscope-dev`
          # and browser tests work) plus wasm toolchain, trunk, tailwind,
          # chromium. WEB_DIST points at the prebuilt test bundle so
          # browser tests don't depend on a clean local rebuild.
          web = pkgs.mkShell {
            nativeBuildInputs = [
              toolchain
            ]
            ++ commonTools
            ++ backendNativeBuildInputs
            ++ webNativeBuildInputs;
            buildInputs = backendBuildInputs;
            env = commonEnv // backendEnv // webEnv;
            shellHook = ''
              ${gcRootsPrelude}
              ${pinWikidataRoot}
              ${mkBanner "web" ""}
            '';
          };

          # Analysis crate work: backend stack + Python analysis env +
          # model weights + corpus images. Corpus is mandatory: iterating
          # on analysis correctness without the corpus produces tests that
          # don't catch real regressions.
          analysis = pkgs.mkShell {
            nativeBuildInputs = [
              toolchain
              pythonEnvs.analysisEnv
            ]
            ++ commonTools
            ++ backendNativeBuildInputs;
            buildInputs = backendBuildInputs;
            env =
              commonEnv
              // backendEnv
              // pythonEnvs.modelEnv
              // {
                CORPUS_IMAGES = corpus.corpusImages;
              };
            shellHook = ''
              ${gcRootsPrelude}
              ${pinWikidataRoot}
              ${pinWeights}
              ${pinCorpus}
              ${mkBanner "analysis" ''
                echo "  python: $(python3 --version 2>/dev/null)"
              ''}
            '';
          };

          # Triton serving / harness work: Python analysis env + model
          # weights + the rust toolchain (for schematool, used by Python
          # tests for cross-language schema validation). No corpus —
          # corpus correctness lives in `analysis`. No backend native
          # deps — schematool comes prebuilt from rust.packages.default.
          triton = pkgs.mkShell {
            nativeBuildInputs = [
              toolchain
              pythonEnvs.analysisEnv
              rust.packages.default
            ]
            ++ commonTools;
            env = commonEnv // pythonEnvs.modelEnv;
            shellHook = ''
              ${gcRootsPrelude}
              ${pinWeights}
              ${mkBanner "triton" ''
                echo "  python: $(python3 --version 2>/dev/null)"
                echo "  schematool: $(command -v schematool 2>/dev/null || echo 'not found')"
              ''}
            '';
          };

        }
        // lib.optionalAttrs isDarwin {
          # iOS project generation & Swift lint/format. The OpenAPI spec is
          # prebuilt via `nix build .#openapi`.
          ios = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              just
              xcodegen
              swiftformat
              swiftlint
              xcbeautify
            ];
            shellHook = ''
              ${gcRootsPrelude}
              ${mkBanner "ios" ''
                echo "  xcodegen: $(xcodegen --version 2>/dev/null)"
              ''}
            '';
          };
        };
      }
    );
}
