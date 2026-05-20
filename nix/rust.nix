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

in
{
  checks = {
    fmt = craneLib.cargoFmt {
      inherit src;
      pname = "chronoscope";
      version = "0.1.0";
    };

    clippy = craneLib.cargoClippy (
      checkArgs
      // {
        cargoClippyExtraArgs = "--all-targets -- -D warnings";
      }
    );

    # Workspace test run. The `chronoscope-dev::tests/web.rs` target is
    # gated behind a `required-features = ["browser-tests"]` entry in
    # `dev/Cargo.toml`, so this run silently skips it — the dedicated
    # `web-test` check below picks it up with capped parallelism. Keeping
    # the heavy Chrome-driven tests out of this derivation lets the rest
    # of the workspace's tests run at full cargo parallelism without
    # Chrome processes co-contending for the cores.
    test = craneLib.cargoTest (checkArgs // testExtraEnv);

    # Dedicated check for the browser test suite, with `--test-threads=4`.
    # More concurrent Chromes than that starve `chromiumoxide`'s CDP-response
    # budget under the Nix sandbox and tests fail with "Error: Timeout"
    # rather than an actual assertion. Four is the empirical sweet spot —
    # single-digit Chromes per box, still ~4× faster than --test-threads=1.
    web-test = craneLib.cargoTest (
      checkArgs
      // testExtraEnv
      // {
        pname = "chronoscope-web-tests";
        cargoTestExtraArgs = "-p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4";
      }
    );

    llvm-cov = craneLib.cargoLlvmCov (
      checkArgs
      // testExtraEnv
      // {
        # crane's cargoLlvmCov sets installPhaseCommand="" and expects the
        # coverage command to write $out directly. --output-path $out writes
        # the LCOV report as a file at $out (not a directory).
        cargoLlvmCovExtraArgs = "--fail-under-lines 75 --lcov --output-path $out";
        nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
          pkgs.cargo-llvm-cov
        ];
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
