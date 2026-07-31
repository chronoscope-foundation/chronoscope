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
  #
  # The `.md` clause carries `web/content/`, which `web/build.rs` reads: drop it
  # and every web build fails on a missing `include_str!` in `build/render.rs`.
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

  # Self-hosted webfonts, subsetted and converted to woff2.
  #
  # These used to load from fonts.googleapis.com at runtime, which put a
  # third-party request on the critical render path, handed every visitor's IP to
  # Google, and left the page reflowing when the real face arrived (~15 px on the
  # nav wordmark). Serving them same-origin also makes measurement reproducible:
  # the browser tests run without egress, so they were laying out in a fallback
  # serif and disagreeing with what a developer saw.
  #
  # Fetched file-by-file rather than from nixpkgs' `google-fonts`, whose closure
  # is 1.8 GB — too much to drag onto the gate's critical path for four files.
  # Each fetch is content-pinned, so an upstream change fails the build loudly
  # instead of silently altering the typography.
  #
  # `google-fonts` does take a `fonts` argument, and reaching for
  # `google-fonts.override { fonts = [ ... ]; }` looks like the tidier answer.
  # It is not: `fonts` filters what gets *installed*, not what gets *fetched*,
  # and the source is the entire google/fonts repository at 2.1 GB. Overriding
  # also changes the derivation hash, so nothing substitutes and the whole thing
  # must be built locally. That turns an avoidable 1.8 GB cache fetch into an
  # unavoidable 2.1 GB one, on the path every cold `just check` takes. Nor are
  # there per-font outputs to depend on: the package has exactly `out` and
  # `adobeBlank`.
  #
  # All four are SIL Open Font License, so their OFL texts ship alongside them.
  fontSources = {
    "cormorant" = {
      url = "https://raw.githubusercontent.com/google/fonts/main/ofl/cormorant/Cormorant%5Bwght%5D.ttf";
      hash = "sha256-jxLLIfBbYWSRkur/E+7rG1YZvFJP7q5nL7kWl0JZoHY=";
      licenceHash = "sha256-YHANNRysRlDFHz+dsxjSpCD4tFBS26JxXrX+xB8PaVY=";
      # Family and weight range come from the font's own name and fvar
      # tables. The family must match what `input.css` names in its `@theme`,
      # or the browser quietly falls back.
      family = "Cormorant";
      weight = "300 700";
    };
    "dm-sans" = {
      url = "https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans%5Bopsz%2Cwght%5D.ttf";
      hash = "sha256-jNCNl+icJNCqku3S8PTI7mGV7um3yfFUhlpYsC8MHA0=";
      licenceHash = "sha256-mvNhkDMkN/Xs0Jl03kPB98d6MQqZbN2M6yVii0WIQOE=";
      family = "DM Sans";
      weight = "100 1000";
      # Pinned rather than kept variable. This face is only ever small interface
      # text (roughly 10-16 px: labels, chips, bullets), so its optical-size axis
      # varies nothing a reader could see, while its deltas cost 26 KB. Source
      # Serif keeps its axis because that one spans 12 px bullets to 30 px
      # article headings, which is what `opsz` exists for.
      pinAxes = "opsz=14";
    };
    "source-serif-4" = {
      url = "https://raw.githubusercontent.com/google/fonts/main/ofl/sourceserif4/SourceSerif4%5Bopsz%2Cwght%5D.ttf";
      hash = "sha256-l7LU2m48tJS1oeZq4XaRTYUsyr70ngwCwN8l8+Oaygs=";
      licenceHash = "sha256-X5TD/TojExpBerWgyEUt5X5ww8+59gTYgkH3Bl6/n9k=";
      family = "Source Serif 4";
      weight = "200 900";
    };
    # Carries U+2767, the ornament in the nav drawer. Source Serif has no such
    # glyph, so without this the mark falls back to whatever the platform
    # happens to provide and renders differently per OS.
    "noto-sans-symbols-2" = {
      url = "https://raw.githubusercontent.com/google/fonts/main/ofl/notosanssymbols2/NotoSansSymbols2-Regular.ttf";
      hash = "sha256-fV+3O3ymemeYEBdB9dKAo9AWpWoZevzUGZ27V7S4KiE=";
      licenceHash = "sha256-sRjdQTN4BqXUeXBSx3yvO9CWrteD5eshtNERVDUeGsA=";
      family = "Chronoscope Ornaments";
      weight = "400";
    };
  };

  # One subset per unicode range, mirroring what the Google CDN served.
  #
  # A single bundled file would have been simpler but wrong: entity names come
  # from Wikidata and can carry any script, and the CDN's per-range files let the
  # browser fetch only what a page actually needs. Collapsing that into one file
  # either ships latin-ext to everyone or drops it and falls back to a system
  # font mid-sentence. The `@font-face` rules in `input.css` carry the matching
  # `unicode-range`, which is what lets the browser make that choice.
  #
  # `extras` are marks the interface draws itself (arrows in links and the back
  # button, the dismiss cross) which sit outside the latin range, so they ride
  # along with latin rather than being fetched conditionally.
  fontRanges =
    let
      extras = "U+2190,U+2192,U+2197,U+2264-2265,U+2715";
    in
    {
      latin = "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+2074,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD,${extras}";
      latin-ext = "U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF";
    };

  # Only the OpenType features the interface asks for. `*` keeps every
  # discretionary set a family ships — small caps, oldstyle figures, alternates —
  # which cost Source Serif alone well over 100 KB. `tnum` is here because the
  # year readout is `tabular-nums`, and without it the digits change width as the
  # slider scrubs.
  layoutFeatures = "kern,liga,clig,calt,tnum,ccmp,mark,mkmk,locl,rlig";

  fonts =
    let
      fetched = lib.mapAttrs (
        name: spec:
        pkgs.fetchurl {
          inherit (spec) url hash;
          name = "${name}.ttf";
        }
      ) fontSources;
      # The OFL sits beside the font upstream, so its URL follows from the
      # font's and the family directory is spelled once.
      licences = lib.mapAttrs (
        name: spec:
        pkgs.fetchurl {
          url = "${builtins.dirOf spec.url}/OFL.txt";
          hash = spec.licenceHash;
          name = "OFL-${name}.txt";
        }
      ) fontSources;
      textFamilies = lib.filterAttrs (name: _: name != "noto-sans-symbols-2") fetched;
      # A family's subsetting input: the fetched file, or an instance of it with
      # the axes in `pinAxes` frozen so their deltas drop out.
      prepareCmd = name: spec: ''
        ${
          if spec ? pinAxes then
            ''fonttools varLib.instancer ${fetched.${name}} ${spec.pinAxes} -o "work/${name}-src.ttf"''
          else
            ''cp ${fetched.${name}} "work/${name}-src.ttf"''
        }
      '';
      subsetCmd = outName: src: unicodes: ''
        pyftsubset ${src} \
          --output-file="work/${outName}.ttf" \
          --unicodes="${unicodes}" \
          --layout-features="${layoutFeatures}"
        woff2_compress "work/${outName}.ttf"
        cp "work/${outName}.woff2" $out/
      '';
    in
    pkgs.runCommand "chronoscope-web-fonts"
      {
        nativeBuildInputs = [
          pkgs.woff2
          (pkgs.python3.withPackages (ps: [ ps.fonttools ]))
        ];
      }
      ''
        mkdir -p $out work

        ${lib.concatStringsSep "\n" (lib.mapAttrsToList prepareCmd fontSources)}

        ${lib.concatStringsSep "\n" (
          lib.flatten (
            lib.mapAttrsToList (
              family: _:
              # The weight axis survives subsetting, so one file per range covers
              # every weight the site asks for.
              lib.mapAttrsToList (
                range: unicodes: subsetCmd "${family}-${range}" "work/${family}-src.ttf" unicodes
              ) fontRanges
            ) textFamilies
          )
        )}

        # The ornament is a single codepoint, so it needs no range split.
        ${subsetCmd "noto-sans-symbols-2" "work/noto-sans-symbols-2-src.ttf" "U+2767"}

        ${lib.concatStringsSep "\n" (
          lib.mapAttrsToList (name: file: ''
            cp ${file} $out/OFL-${name}.txt
          '') licences
        )}
        # Generated here rather than hand-written in `input.css` so the unicode
        # ranges have one home. Split across two files they would drift, and a
        # drifted range fails silently: the browser simply stops using the font
        # for the characters the CSS forgot to claim.
        {
          echo "/* Generated by nix/web.nix. Do not edit. */"
          cat <<'CSS'
        ${lib.concatStringsSep "\n" (
          lib.flatten (
            lib.mapAttrsToList (
              family: spec:
              lib.mapAttrsToList (range: unicodes: ''
                @font-face {
                  font-family: "${spec.family}";
                  src: url("${family}-${range}.woff2") format("woff2");
                  font-weight: ${spec.weight};
                  font-style: normal;
                  font-display: swap;
                  unicode-range: ${unicodes};
                }
              '') fontRanges
            ) (lib.filterAttrs (n: _: n != "noto-sans-symbols-2") fontSources)
          )
        )}
        @font-face {
          font-family: "${fontSources."noto-sans-symbols-2".family}";
          src: url("noto-sans-symbols-2.woff2") format("woff2");
          font-weight: ${fontSources."noto-sans-symbols-2".weight};
          font-style: normal;
          font-display: swap;
          unicode-range: U+2767;
        }
        CSS
        } > $out/fonts.css
      '';

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

        # Self-hosted fonts, at the same /fonts/ path the dev server serves them
        # from, so one set of @font-face rules works in both.
        mkdir -p $out/fonts
        cp ${fonts}/* $out/fonts/

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

  # The crate's plain #[test]s (e.g. the build-time render suite in
  # `web/build/render.rs`, which `web/tests/content_render.rs` gives a runner)
  # run on the host, not wasm — a workspace `cargo test` never reaches them
  # because chronoscope-web is excluded from default-members. Built with the native
  # craneLib against webSrc, because the shared workspace filter keeps only
  # cargo sources and every web build needs the `.md` content pages that
  # `web/build.rs` renders. Test-profile deps-only so the release LTO profile
  # doesn't recompile the dependency tree.
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
    # Exposed so the dev shell can point Trunk at the same subsetted files the
    # production dist ships, rather than the two paths drifting.
    web-fonts = fonts;
  };

  checks = {
    web-build = web;
    web-test-build = webTest;
    web-clippy = webClippy;
    web-native-test = webNativeTest;
    web-native-clippy = webNativeClippy;
  };
}
