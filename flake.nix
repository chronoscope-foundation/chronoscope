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

    # Container images as manifests over store paths: a code change pushes the
    # layers that changed, not a fresh multi-hundred-MB tarball per build.
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Infrastructure as Nix modules instead of HCL, compiled to the
    # config.tf.json OpenTofu reads. OpenTofu rather than Terraform: the latter
    # is BSL, which nixpkgs marks unfree.
    terranix = {
      url = "github:terranix/terranix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # OpenHistoricalMap's built basemap style, tag v0.9.17, pinned by the commit
    # that tag names. We serve this document ourselves, so the basemap the map
    # draws moves when this line moves: bump the SHA, `nix flake update
    # ohm-style`, read the lock diff.
    #
    # `type = "file"` fetches the one document (254 KB) rather than the
    # repository (85 MB). The tiles, glyphs and sprites it names by absolute URL
    # stay OHM's own.
    ohm-style = {
      url = "https://raw.githubusercontent.com/OpenHistoricalMap/map-styles/59503aac5f5dc5f0afac2ce326d55ef9e575b33b/dist/historical/historical.json";
      flake = false;
      type = "file";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      crane,
      flake-utils,
      nix2container,
      terranix,
      ohm-style,
      ...
    }:
    let
      # libspatialite's per-connection close hook calls `xmlCleanupParser()`,
      # which libxml2 defines as process-global teardown to be run once at exit
      # with no other thread inside libxml2. SQLite loads and unloads the
      # extension per connection, so any pool with churn runs that global
      # teardown while sibling connections are still using libxml2, and a
      # thread blocks forever on the catalog mutex being torn down under it.
      # Measured: 24/24 hangs with libxml2 linked, 0/24 without, on both
      # x86_64-linux and aarch64, in a threaded open/load/close loop.
      #
      # An overlay rather than a per-callsite override so every consumer (the
      # crate buildInputs, SPATIALITE_LIBRARY_PATH, the image, the dev shells)
      # resolves the same library. A mixture would be undetectable.
      #
      # Costs XmlBLOB support (`XB_*`, ISO metadata), unused here. Revert once
      # upstream stops running a process-global cleanup per connection.
      spatialiteWithoutLibxml2 = _final: prev: {
        libspatialite = prev.libspatialite.overrideAttrs (old: {
          configureFlags = old.configureFlags ++ [ "--disable-libxml2" ];
          buildInputs = builtins.filter (p: !(prev.lib.hasInfix "libxml2" (toString p))) old.buildInputs;
        });
      };
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ spatialiteWithoutLibxml2 ];
        };
        inherit (pkgs) lib;
        inherit (pkgs.stdenv.hostPlatform) isDarwin isLinux;

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
        # + .json (test fixtures, schemas) + .html/.css (web frontend)
        # + proptest-regressions/*.txt. The last are the counterexamples proptest
        # pins when a property fails; left out, the shrunk case that motivated a
        # fix is replayed on every developer's machine and never in the gate.
        srcFilterBase =
          path: type:
          (craneLib.filterCargoSources path type)
          || (lib.hasSuffix ".proto" path)
          || (lib.hasSuffix ".sql" path)
          || (lib.hasSuffix ".json" path)
          || (lib.hasSuffix ".html" path)
          || (lib.hasSuffix ".css" path)
          || (lib.hasInfix "/proptest-regressions/" path && lib.hasSuffix ".txt" path);

        src = lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter = srcFilterBase;
          name = "chronoscope-source";
        };

        workspaceSrc = import ./nix/workspace-src.nix { inherit lib pkgs craneLib; };

        # Artifacts built from one package of the workspace, each with the
        # crates it compiles and the non-cargo files its build reads.
        #
        # Registered together rather than beside each build so `workspace-
        # closures` covers every one of them: a deployable gets the guarantee
        # by being listed here, not by someone remembering to check it.
        deployables = {
          api = rec {
            package = "chronoscope-api";
            crates = [
              "analysis"
              "api"
              "api-client"
              "core"
              "db"
              "integrations"
              "macros"
            ];
            # Compile-time inputs, both: `sqlx::migrate!` embeds the .sql under
            # db/migrations*/, and analysis/build.rs compiles analysis/proto/.
            # The .json fixtures under integrations/ are not, since the binary
            # builds with doCheck = false, so editing a test fixture leaves the
            # production image alone.
            extra = map (workspaceSrc.withExtensions [
              "sql"
              "proto"
            ]) (map (crate: ./. + "/${crate}") crates);
          };

          web = {
            package = "chronoscope-web";
            crates = [
              "api-client"
              "core"
              "macros"
              "web"
            ];
            extra = [
              # Read by the clippy checks sharing this source. It arrived
              # incidentally before, when the filter kept every .toml.
              ./clippy.toml
              # trunk's entry point, tailwind's input, any hand-written script.
              (workspaceSrc.withExtensions [ "html" "css" "js" ] ./web)
              # web/build.rs include_str!s these. Scoped to content/ rather
              # than all of web/, so prose does not rebuild the bundle.
              (workspaceSrc.withExtensions [ "md" ] ./web/content)
            ];
          };
        };

        deployableSrcs = lib.mapAttrs (
          name: deployable:
          workspaceSrc.mkWorkspaceSrc {
            name = "chronoscope-${name}-source";
            root = ./.;
            fullSrc = src;
            inherit (deployable) crates extra;
          }
        ) deployables;

        # The `doc` check builds the whole workspace incl. chronoscope-web on
        # the host target, and web include_str!s page content (.md) from web/.
        # Superset of `src`; scoped to web/ so doc/README markdown edits don't
        # churn the other checks' source hash.
        docSrc = lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter =
            path: type: srcFilterBase path type || (lib.hasInfix "/web/" path && lib.hasSuffix ".md" path);
          name = "chronoscope-doc-source";
        };

        rust = import ./nix/rust.nix {
          inherit
            pkgs
            craneLib
            lib
            src
            docSrc
            postgresWithPostgis
            ;
          # Lazy: only `test`/`llvm-cov` force these, so the wikidata/web
          # cycle stays unresolved at eval time.
          testExtraEnv = apiRuntimeEnv // webEnv;
        };

        pythonEnvs = import ./nix/python.nix { inherit pkgs lib; };

        vision = import ./nix/vision.nix {
          inherit pkgs lib;
          inherit (pythonEnvs) sam3Cache;
        };

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
            spatialitePreload
            ;
          rustCommonArgs = rust.commonArgs;
          inherit (rust) cargoArtifacts;
        };

        # Where in the served bundle the pinned basemap style is staged. The
        # production dist and the dev server both stage it from here, and
        # `BASEMAP.style_url` in web/src/components/map.rs asks for this same
        # path, rooted at the page.
        ohmStylePath = "basemap/ohm-historical.json";

        web = import ./nix/web.nix {
          inherit
            pkgs
            lib
            fenix
            crane
            craneLib
            system
            ohmStylePath
            ;
          webSrc = deployableSrcs.web;
          # Native (non-wasm) crane bits for the host-target `web-native-test`
          # and `web-native-clippy` checks, which run and lint the crate's plain
          # #[test]s. The wasm pipeline covers only what ships.
          rustCommonArgs = rust.commonArgs;
          ohmStyle = ohm-style;
        };

        api = import ./nix/api.nix {
          inherit pkgs craneLib spatialitePreload;
          apiSrc = deployableSrcs.api;
          rustCommonArgs = rust.commonArgs;
          inherit (rust) cargoArtifacts;
        };

        # The curated facts DB rides in the image, so a fresh instance serves
        # the moment it boots instead of waiting on an external mount.
        ociImage = import ./nix/oci.nix {
          inherit pkgs lib;
          inherit (nix2container.packages.${system}) nix2container;
          inherit (api) apiBin runtimeEnv;
          factsDb = wikidata.factsDbs.curated;
        };

        infra = import ./nix/infra.nix {
          inherit pkgs terranix;
          settings = import ./nix/infra-settings.nix;
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

        pythonChecks = pythonEnvs.checks;

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

        # PostgreSQL + PostGIS for the ephemeral-cluster harness: puts
        # initdb/pg_ctl/psql on PATH with postgis loadable. Feeds the api dev
        # shell (local `cargo test -p chronoscope-db --features postgres`) and
        # the hermetic `postgres-smoke` and `api-postgres` checks. The major is
        # the one nix/infra.nix declares on Cloud SQL, so what the gate
        # exercises is the planner production runs; the two move together.
        postgresWithPostgis = pkgs.postgresql_18.withPackages (p: [ p.postgis ]);

        # Preload libspatialite so it and its C++ deps (PROJ/GEOS) stay mapped
        # for the whole process. SQLite dlcloses the mod_spatialite extension at
        # connection close; without the preload those deps unload mid-run and
        # their static destructors fault at exit, a known SQLite
        # loadable-extension teardown bug. mod_spatialite is a Mach-O bundle
        # (unpreloadable), so we name libspatialite, the sibling library that
        # links the same deps, and let the loader follow its NEEDED list.
        #
        # Darwin-only: validated there (DYLD_INSERT_LIBRARIES). The Linux
        # analog was measured and rejected: LD_PRELOAD of libspatialite.so
        # faults on load, and pinning mod_spatialite resident leaves the
        # libxml2 teardown hang untouched, which the overlay above settles at
        # the source instead.
        spatialitePreload = lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          DYLD_INSERT_LIBRARIES = "${pkgs.libspatialite}/lib/libspatialite.dylib";
        };
        # Runtime env for the api/db/ingestion code paths (consumed by the
        # backend/web dev shells AND by the hermetic test/llvm-cov checks).
        # The server's own share of it comes from nix/api.nix, which is also
        # what the wrapper script and the image config carry, so a new variable
        # reaches all four at once. The entities snapshot is the ingestion
        # tooling's, not the server's, so it is added here.
        apiRuntimeEnv = api.runtimeEnv // {
          WIKIDATA_ENTITIES_JSONL = "${wikidata.bundles.curated.entities}/entities.jsonl";
        };
        backendEnv =
          apiRuntimeEnv
          // {
            PROTOC = "${pkgs.protobuf}/bin/protoc";
          }
          // lib.optionalAttrs isLinux {
            # Carries nix/rust.nix's reason into the shells: cargo here links the
            # same way the hermetic builds do, so a `cargo run` produces a binary
            # with the same RPATH rather than one that dies at exec. wasm32 keeps
            # lld regardless, so the web shell is unaffected.
            RUSTFLAGS = "-Clinker-features=-lld";
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
          # Trunk's pre_build hook stages these into web/fonts so the dev server
          # serves exactly what the production dist does.
          CHRONOSCOPE_WEB_FONTS = web.packages.web-fonts;
          # Likewise for the pinned basemap style, which the map loads from our
          # own origin in all three serving environments. Trunk's post_build
          # hook stages the document at the path the dist puts it at.
          CHRONOSCOPE_OHM_STYLE = "${ohm-style}";
          CHRONOSCOPE_OHM_STYLE_PATH = ohmStylePath;
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

            # Compiles the terranix modules and holds the result to the
            # providers' own schemas, so an infrastructure change that cannot
            # apply fails here instead of halfway through an apply.
            infra-validate = infra.validate;

            workspace-closures = workspaceSrc.mkClosureCheck {
              inherit src deployables;
              cargo = toolchain;
            };

            # Hermetic coverage of the build-db path: a dump-free facts DB from
            # the curated bundle, self-validating that it holds facts.
            wikidata-facts-db-curated = wikidata.factsDbs.curated;
            # corpus-tests intentionally excluded — requires GPU (run on
            # dedicated CI runners via `nix build .#corpus-tests`).
          }
          // lib.optionalAttrs isDarwin {
            ios-project-spec = openapi.projectSpec;
          }
          // lib.optionalAttrs isLinux {
            # Builds the image and runs its entrypoint, so a container that
            # cannot start fails a check rather than a deploy. Only the current
            # system's checks run, so a darwin machine reaches this through
            # `just check linux`, which asks for the x86_64-linux outputs by
            # name.
            oci-api-boots = ociImage.boots;
          };

        packages =
          rust.packages
          // web.packages
          // {
            inherit (api) api;
            openapi = openapi.spec;

            # The generated OpenTofu config. `just infra-plan` stages this file
            # into the working directory it runs tofu from.
            infra-config = infra.tfConfig;

            corpus-images = corpus.corpusImages;
            corpus-fetch = corpus.corpusFetchBin;
            corpus-tests = corpus.corpusTests;
            analysis-results = corpus.analysisResults;

            # Model weights — built with --impure and HF_TOKEN to populate store.
            dinov3-weights = pythonEnvs.dinov3Repo;
            sam3-weights = pythonEnvs.sam3Cache;

            # ONNX exports. Packages, not checks: multi-gigabyte, and the
            # weight FODs they consume need HF_TOKEN on a machine whose store
            # lacks them, which a pure `nix flake check` cannot supply.
            sam3-onnx = vision.sam3Onnx;
            vision-export-env = vision.exportEnv;

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
          }
          // lib.optionalAttrs isLinux {
            # Built cross-system from a dev machine: `nix build
            # .#packages.x86_64-linux.oci-api`.
            oci-api = ociImage.image;
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
            buildInputs = backendBuildInputs ++ [ postgresWithPostgis ];
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

          # Pushing images and rolling Cloud Run. gcloud and skopeo move the
          # artifact `nix build` already produced; tofu is what rolls the
          # service onto it, since the service's shape is declared and a deploy
          # only hands over the digest. skopeo-nix2container is the skopeo that
          # speaks the `nix:` transport, letting a push stream the manifest's
          # store paths straight to the registry.
          deploy = pkgs.mkShell {
            nativeBuildInputs = [
              pkgs.just
              pkgs.google-cloud-sdk
              nix2container.packages.${system}.skopeo-nix2container
              infra.tofu
            ];
            shellHook = ''
              ${mkBanner "deploy" ''
                echo "  gcloud: $(gcloud --version 2>/dev/null | head -n1)"
              ''}
            '';
          };

          # Declaring cloud resources. A subset of `deploy`: planning or
          # applying an infrastructure change publishes no image, so it leaves
          # out the tools that push one. gcloud is here for the
          # application-default credentials the provider authenticates with.
          infra = pkgs.mkShell {
            nativeBuildInputs = [
              pkgs.just
              pkgs.google-cloud-sdk
              infra.tofu
            ];
            shellHook = ''
              ${mkBanner "infra" ''
                echo "  tofu: $(tofu --version 2>/dev/null | head -n1)"
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
