# Hermetic WASM build for chronoscope-web.
#
# Pipeline: crane (wasm32) → wasm-bindgen → wasm-opt → tailwindcss → assemble dist/
#
# Two outputs:
#   - `web`      — production build (no test-hooks feature).
#   - `web-test` — same pipeline with `--features test-hooks` for browser tests.
# Both share `cargoArtifacts` because the test-hooks feature is purely additive
# instrumentation that does not alter dependency resolution.
{
  pkgs,
  lib,
  fenix,
  crane,
  system,
  src,
}:

let
  # WASM-only toolchain — no openssl/sqlite needed.
  wasmToolchain =
    with fenix.packages.${system};
    combine [
      stable.cargo
      stable.rustc
      targets.wasm32-unknown-unknown.stable.rust-std
    ];

  wasmCraneLib = (crane.mkLib pkgs).overrideToolchain wasmToolchain;

  # Filter source: Cargo files from the whole workspace (crane needs root
  # Cargo.toml/Cargo.lock), but web-specific assets only from web/.
  webSrc = lib.cleanSourceWith {
    src = wasmCraneLib.path (toString ../. + "/.");
    filter =
      path: type:
      (wasmCraneLib.filterCargoSources path type)
      || (
        lib.hasInfix "/web/" path
        && (
          lib.hasSuffix ".html" path
          || lib.hasSuffix ".css" path
          || lib.hasSuffix ".md" path
          || lib.hasSuffix ".js" path
        )
      );
    name = "chronoscope-web-source";
  };

  commonArgs = {
    src = webSrc;
    pname = "chronoscope-web";
    version = "0.1.0";
    strictDeps = true;

    # Build only the web crate for WASM target.
    cargoExtraArgs = "-p chronoscope-web --target wasm32-unknown-unknown";

    # WASM target does not need native deps.
    nativeBuildInputs = [ ];
    buildInputs = [ ];

    # Don't try to run WASM tests on native.
    doCheck = false;
  };

  cargoArtifacts = wasmCraneLib.buildDepsOnly commonArgs;

  # Build the WASM binary, optionally with extra cargo features.
  mkWasmBuild =
    {
      pname,
      extraCargoArgs ? "",
    }:
    wasmCraneLib.buildPackage (
      commonArgs
      // {
        inherit cargoArtifacts pname;
        cargoExtraArgs = commonArgs.cargoExtraArgs + extraCargoArgs;
        # crane tries to install binaries; WASM produces a .wasm, not an executable.
        # Override install to just copy the target output.
        installPhaseCommand = ''
          mkdir -p $out/lib
          cp target/wasm32-unknown-unknown/release/chronoscope_web.wasm $out/lib/ 2>/dev/null \
            || cp target/wasm32-unknown-unknown/release/chronoscope-web.wasm $out/lib/chronoscope_web.wasm
        '';
      }
    );

  # Post-process and assemble a final dist/ output from a wasm build.
  mkDist =
    {
      name,
      wasmBuild,
    }:
    pkgs.runCommand name
      {
        nativeBuildInputs = with pkgs; [
          wasm-bindgen-cli
          binaryen
          tailwindcss_4
        ];
      }
      ''
        mkdir -p $out work

        # wasm-bindgen post-processes the .wasm into JS glue + optimized wasm
        wasm-bindgen \
          --target web \
          --out-dir work \
          --out-name chronoscope_web \
          ${wasmBuild}/lib/chronoscope_web.wasm

        # wasm-opt shrinks the binary; --all-features accepts whatever WASM
        # features rustc emits (bulk-memory, mutable-globals, etc.)
        wasm-opt -Oz --all-features -o work/chronoscope_web_bg_opt.wasm work/chronoscope_web_bg.wasm
        mv work/chronoscope_web_bg_opt.wasm work/chronoscope_web_bg.wasm

        # Tailwind CSS (v4 auto-detects content via @source in input.css).
        # Copy web source to a writable directory — tailwindcss needs to write
        # intermediate files next to the source.
        cp -r ${webSrc}/web tw-work
        chmod -R u+w tw-work
        tailwindcss \
          -i input.css \
          -o "$out/tailwind.css" \
          --minify \
          --cwd tw-work

        # Assemble dist/
        cp work/chronoscope_web_bg.wasm $out/
        cp work/chronoscope_web.js $out/

        # Process index.html — replace Trunk data attributes with direct references
        sed \
          -e 's|<link data-trunk rel="css" href="tailwind.css" />|<link rel="stylesheet" href="tailwind.css" />|' \
          -e 's|<link data-trunk rel="rust" data-wasm-opt="z" />|<script type="module">import init from "./chronoscope_web.js"; init();</script>|' \
          ${webSrc}/web/index.html > $out/index.html
      '';

  wasmBuild = mkWasmBuild { pname = "chronoscope-web"; };
  wasmBuildTest = mkWasmBuild {
    pname = "chronoscope-web-test";
    extraCargoArgs = " --features test-hooks";
  };

  web = mkDist {
    name = "chronoscope-web-dist";
    inherit wasmBuild;
  };

  webTest = mkDist {
    name = "chronoscope-web-dist-test";
    wasmBuild = wasmBuildTest;
  };

in
{
  packages = {
    inherit web;
    web-test = webTest;
  };

  checks = {
    web-build = web;
    web-test-build = webTest;
  };
}
