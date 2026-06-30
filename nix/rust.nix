{
  pkgs,
  craneLib,
  lib,
  src,
  # Defaulting to {} keeps this module loadable before regions/wikidata exist
  # — see flake.nix for how the lazy cycle resolves.
  testExtraEnv ? { },
}:

let
  commonArgs = {
    inherit src;
    pname = "chronoscope";
    version = "0.1.0";
    strictDeps = true;

    # Nix-provided protoc — tonic-prost-build finds it via this env var.
    PROTOC = "${pkgs.protobuf}/bin/protoc";

    nativeBuildInputs = with pkgs; [
      pkg-config
    ];

    # openssl: webauthn-rs depends on openssl-sys unconditionally (all platforms).
    # libspatialite: loaded at runtime via SELECT load_extension() for
    # spatial queries (region assignment, point-in-polygon).
    buildInputs =
      with pkgs;
      [
        sqlite
        openssl
        libspatialite
        geos # cosmogony (region-builder dep) links against GEOS
      ]
      ++ lib.optionals stdenv.hostPlatform.isDarwin [
        libiconv
      ];
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # Shared by every check derivation. The workspace [profile.release]
  # (LTO + opt-level=z + codegen-units=1) makes test-binary linking take
  # many minutes; the test-profile cache keeps `just check` snappy.
  checkArgs = commonArgs // {
    CARGO_PROFILE = "test";
    cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { CARGO_PROFILE = "test"; });
  };

  clippy = craneLib.cargoClippy (
    checkArgs
    // {
      cargoClippyExtraArgs = "--all-targets -- -D warnings";
    }
  );

  # Doctests get their own check because `cargo llvm-cov` (stable) skips them.
  # They guard real invariants — e.g. the `compile_fail` grammar-macro examples
  # in `chronoscope-macros` and `core/facts`.
  doctest = craneLib.cargoTest (
    checkArgs
    // testExtraEnv
    // {
      pname = "chronoscope-doctest";
      cargoTestExtraArgs = "--doc";
    }
  );

  # Coverage doubles as the workspace test run: it executes the unit +
  # integration suite (a failing test fails the check) and enforces the line
  # threshold, so there is no separate plain test derivation. The
  # `chronoscope-dev::tests/web.rs` target is gated behind `browser-tests` in
  # `dev/Cargo.toml`, so it's skipped here; the dedicated `web-test` check runs
  # the Chrome-driven tests with capped parallelism.
  #
  # crane's cargoLlvmCov sets installPhaseCommand="" and expects the command to
  # write $out; --output-path $out puts the LCOV report there.
  llvm-cov = craneLib.cargoLlvmCov (
    checkArgs
    // testExtraEnv
    // {
      cargoLlvmCovExtraArgs = "--fail-under-lines 75 --lcov --output-path $out";
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
        pkgs.cargo-llvm-cov
      ];
    }
  );
in
{
  checks = {
    fmt = craneLib.cargoFmt {
      inherit src;
      pname = "chronoscope";
      version = "0.1.0";
    };

    inherit clippy doctest llvm-cov;

    # Dedicated check for the browser test suite, with `--test-threads=4`.
    # More concurrent Chromes than that starve `chromiumoxide`'s CDP-response
    # budget and tests fail with "Error: Timeout" rather than an actual
    # assertion. Four is the empirical sweet spot — single-digit Chromes per
    # box, still ~4× faster than --test-threads=1.
    #
    # That starvation also strikes *across* derivations: run concurrently with
    # the compile- and coverage-heavy checks under `nix flake check`, the
    # browser tests stall and time out (they pass reliably with the box to
    # themselves — cf. `just test web`). Referencing those checks' outputs
    # orders this derivation after them, so the browser tests run unstarved;
    # the reference only establishes build order.
    web-test = craneLib.cargoTest (
      checkArgs
      // testExtraEnv
      // {
        pname = "chronoscope-web-tests";
        cargoTestExtraArgs = "-p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4";
        CHRONOSCOPE_RUN_AFTER = "${clippy} ${doctest} ${llvm-cov}";
      }
    );
  };

  packages = {
    default = craneLib.buildPackage (
      commonArgs
      // {
        inherit cargoArtifacts;
        doCheck = false;
      }
    );
  };

  inherit commonArgs cargoArtifacts;
}
