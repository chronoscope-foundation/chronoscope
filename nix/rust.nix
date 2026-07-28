{
  pkgs,
  craneLib,
  lib,
  src,
  # Whole-workspace source (adds web's include_str!'d assets) for the `doc`
  # check, which documents chronoscope-web alongside the native crates.
  docSrc,
  # PostgreSQL + PostGIS bundle for the `postgres-smoke` check (initdb/pg_ctl
  # on PATH). Only that check forces it, so it stays lazy like testExtraEnv.
  postgresWithPostgis,
  # Defaulting to {} keeps this module loadable before wikidata/web exist
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

  # `--all-features` rather than a list of features to lint: the compile-time
  # gates cover everything by default, so a feature added later is linted
  # without anyone remembering to add it here. Opting features in one at a time
  # is how `db`'s postgres backend went unlinted from its first commit — a bare
  # `unwrap()` in it passed this check clean, because the module never compiled
  # under clippy at all, and how `browser-tests` (which gates `dev/tests/web.rs`
  # into existence) had to be named explicitly to lint that target at all.
  clippy = craneLib.cargoClippy (
    checkArgs
    // {
      cargoClippyExtraArgs = "--all-targets --all-features -- -D warnings";
    }
  );

  # Doctests get their own check because `cargo llvm-cov` (stable) skips them.
  # They guard real invariants — e.g. the `compile_fail` grammar-macro examples
  # in `chronoscope-macros` and `core/facts`.
  #
  # Default features for the same reason as llvm-cov below: this one runs what
  # it finds. A doctest behind a feature belongs to whichever check already
  # supplies that feature's resources.
  doctest = craneLib.cargoTest (
    checkArgs
    // testExtraEnv
    // {
      pname = "chronoscope-doctest";
      cargoTestExtraArgs = "--doc";
    }
  );

  # Rustdoc over the whole workspace (`--workspace` pulls in chronoscope-web,
  # which default-members excludes; it documents fine on the host target).
  # `-D warnings` is the doc analog of clippy's `-D warnings`: broken and
  # private intra-doc links, redundant link targets, and bad HTML all fail the
  # gate. `--no-deps` keeps it to first-party crates; links into dependencies
  # still resolve.
  doc = craneLib.cargoDoc (
    checkArgs
    // {
      pname = "chronoscope-doc";
      src = docSrc;
      cargoDocExtraArgs = "--no-deps --workspace --all-features";
      RUSTDOCFLAGS = "-D warnings";
    }
  );

  # Coverage doubles as the workspace test run: it executes the unit +
  # integration suite (a failing test fails the check) and enforces the line
  # threshold, so there is no separate plain test derivation. The
  # `chronoscope-dev::tests/web.rs` target is gated behind `browser-tests` in
  # `dev/Cargo.toml`, so it's skipped here; the dedicated `web-test` check runs
  # the Chrome-driven tests with capped parallelism.
  #
  # Default features, deliberately — do not harmonize this with clippy's
  # `--all-features`. Three features gate suites that must not run here:
  # `browser-tests` needs the box to itself, `corpus-test` needs the fetched
  # corpus, and `record-fixtures` hits the real network. Linting everything is
  # free; running everything is not.
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

  # The db crate's postgres conformance suite over an ephemeral postgres+PostGIS
  # cluster (initdb/pg_ctl on PATH via nativeBuildInputs). Compile- and
  # cluster-heavy, so it is let-bound like the other heavy checks and ordered
  # before `web-test` (see `CHRONOSCOPE_RUN_AFTER` there): left co-schedulable,
  # it would starve the headless-Chrome CDP event loop the same way.
  #
  # `pname` is deliberately short: it lengthens the build dir, and the unix-socket
  # path is capped at ~104 bytes on darwin. The harness keeps the socket under
  # PG_SOCKET_BASE (default /tmp), so the build-dir depth stays off the socket
  # path — but a short pname is cheap insurance.
  postgres-smoke = craneLib.cargoTest (
    checkArgs
    // {
      pname = "pg-smoke";
      cargoTestExtraArgs = "-p chronoscope-db --features postgres postgres::";
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ postgresWithPostgis ];
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

    inherit
      clippy
      doc
      doctest
      llvm-cov
      postgres-smoke
      ;

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
    # (including the heavy `postgres-smoke`) orders this derivation after them,
    # so the browser tests run unstarved; the reference only establishes build
    # order.
    web-test = craneLib.cargoTest (
      checkArgs
      // testExtraEnv
      // {
        pname = "chronoscope-web-tests";
        cargoTestExtraArgs = "-p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4";
        CHRONOSCOPE_RUN_AFTER = "${clippy} ${doc} ${doctest} ${llvm-cov} ${postgres-smoke}";
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
