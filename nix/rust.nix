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

    test = craneLib.cargoTest (checkArgs // testExtraEnv);

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
