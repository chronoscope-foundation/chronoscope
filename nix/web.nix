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
  craneLib,
  system,
  src,
  rustCommonArgs,
}:

let
  wasmToolchain =
    with fenix.packages.${system};
    combine [
      stable.cargo
      stable.clippy
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
        # Cargo emits chronoscope-web.wasm; downstream wasm-bindgen / JS glue
        # expects snake_case. Rename rather than carry both names.
        installPhaseCommand = ''
          mkdir -p $out/lib
          cp target/wasm32-unknown-unknown/release/chronoscope-web.wasm \
            $out/lib/chronoscope_web.wasm
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

  # Clippy against the wasm32 target with -D warnings, covering the code that
  # ships in the bundle. The workspace-level `clippy` check uses
  # default-members, which excludes chronoscope-web. Without this, web-side lint
  # regressions slip past `nix flake check`. `webNativeClippy` below picks up the
  # crate's test targets.
  webClippy = wasmCraneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-features -- -D warnings";
    }
  );

  # The crate's plain #[test]s (e.g. faq.rs's `parse_faq` suite) run on the
  # host, not wasm — a workspace `cargo test` never reaches them because
  # chronoscope-web is excluded from default-members. Built with the native
  # craneLib against webSrc (the shared workspace filter drops the `.md`
  # content that the pages `include_str!`). Test-profile deps-only so the
  # release LTO profile doesn't recompile the dependency tree.
  webNativeTestDeps = craneLib.buildDepsOnly (
    rustCommonArgs
    // {
      src = webSrc;
      pname = "chronoscope-web-native-test-deps";
      CARGO_PROFILE = "test";
      cargoExtraArgs = "-p chronoscope-web";
    }
  );

  webNativeTest = craneLib.cargoTest (
    rustCommonArgs
    // {
      src = webSrc;
      pname = "chronoscope-web-native-test";
      CARGO_PROFILE = "test";
      cargoArtifacts = webNativeTestDeps;
      cargoExtraArgs = "-p chronoscope-web";
    }
  );

  # `--all-targets` over the host build, so the crate's `#[cfg(test)]` code is
  # held to the workspace lint denials (`unwrap_used`, `panic`, …) as well. The
  # host is where those targets build at all, which `web-native-test` already
  # establishes; this shares its dependency artifacts.
  webNativeClippy = craneLib.cargoClippy (
    rustCommonArgs
    // {
      src = webSrc;
      pname = "chronoscope-web-native-clippy";
      CARGO_PROFILE = "test";
      cargoArtifacts = webNativeTestDeps;
      cargoExtraArgs = "-p chronoscope-web";
      cargoClippyExtraArgs = "--all-targets -- -D warnings";
    }
  );

in
{
  packages = {
    inherit web;
    web-test = webTest;
  };

  checks = {
    web-build = web;
    web-test-build = webTest;
    web-clippy = webClippy;
    web-native-test = webNativeTest;
    web-native-clippy = webNativeClippy;
  };
}
