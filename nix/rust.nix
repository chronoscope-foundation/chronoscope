# Rust workspace builds via crane.
#
# Provides: checks (fmt, clippy, test, llvm-cov), packages, devShell inputs.
{
  pkgs,
  craneLib,
  lib,
  src,
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
    buildInputs =
      with pkgs;
      [
        sqlite
        openssl
      ]
      ++ lib.optionals stdenv.hostPlatform.isDarwin [
        libiconv
      ];
  };

  # Shared dependency artifacts — all check derivations reuse these.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

in
{
  checks = {
    fmt = craneLib.cargoFmt {
      inherit src;
      pname = "chronoscope";
      version = "0.1.0";
    };

    clippy = craneLib.cargoClippy (
      commonArgs
      // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--all-targets -- -D warnings";
      }
    );

    test = craneLib.cargoTest (
      commonArgs
      // {
        inherit cargoArtifacts;
      }
    );

    llvm-cov = craneLib.cargoLlvmCov (
      commonArgs
      // {
        inherit cargoArtifacts;
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
    # All workspace binaries (chronoscope-api, chronoscope-dev, analyze, etc.)
    default = craneLib.buildPackage (
      commonArgs
      // {
        inherit cargoArtifacts;
        doCheck = false; # Tests run as a separate check derivation
      }
    );
  };

  # Shared build args — reused by corpus.nix for corpus test derivations.
  inherit commonArgs;

  # Ingredients for the dev shell (merged in flake.nix).
  devShell = {
    nativeBuildInputs =
      commonArgs.nativeBuildInputs
      ++ (with pkgs; [
        binaryen
        cargo-llvm-cov
        just
        tailwindcss_4
        trunk
        wasm-bindgen-cli
      ])
      # Headless Chrome for browser tests (nixpkgs chromium is Linux-only;
      # on macOS the test harness discovers a system-installed Chrome).
      ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.chromium ];
    inherit (commonArgs) buildInputs;
    env = {
      inherit (commonArgs) PROTOC;
    };
  };
}
