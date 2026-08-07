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
  }
  // lib.optionalAttrs (pkgs.stdenv.hostPlatform.isLinux && pkgs.stdenv.hostPlatform.isx86_64) {
    # rustc 1.90+ links x86_64-unknown-linux-gnu with rust-lld, which bypasses
    # the nixpkgs ld wrapper that turns the link line's `-L /nix/store/...` into
    # RPATH. What comes out runs only where LD_LIBRARY_PATH already names
    # openssl and gcc-lib, so the container dies at exec and cargo's test
    # binaries exit 127. Linking through the wrapped GNU ld keeps RPATH derived
    # from the real link line rather than hand-listed here.
    #
    # Keyed on the architecture rather than the OS: rust-lld is the default only
    # on x86_64, and rustc rejects this flag as unstable on aarch64-linux, so a
    # per-OS guard would break that target the moment anything builds for it.
    RUSTFLAGS = "-Clinker-features=-lld";
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
  # `--all-features`. Two features gate suites that must not run here:
  # `browser-tests` needs the box to itself, and `record-fixtures` hits the
  # real network. Linting everything is free; running everything is not.
  #
  # crane's cargoLlvmCov sets installPhaseCommand="" and expects the command to
  # write $out; --output-path $out puts the LCOV report there.
  llvm-cov = craneLib.cargoLlvmCov (
    checkArgs
    // testExtraEnv
    // {
      # Line coverage sits at 89.6%. At the old floor of 75 the check could
      # only fire after ~5,450 covered lines vanished at once, so the ratchet
      # was not doing its job. 87 fires on roughly a 970-line drop: a large
      # untested module, or a test file deleted. The slack that remains is for
      # the proptests, which seed from entropy per run and move the number
      # between runs on identical code. Raise it as coverage climbs; lower it
      # only deliberately.
      cargoLlvmCovExtraArgs = "--fail-under-lines 87 --lcov --output-path $out";
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
        pkgs.cargo-llvm-cov
      ];

      # Crane unpacks the shared dependency artifact into `target`, and
      # `cargo llvm-cov` builds in `target/llvm-cov-target`. Left alone the
      # artifact is decompressed (48 s) into a directory cargo never reads,
      # and all 486 dependencies are rebuilt into the one it does: 497 crates
      # compiled here against 29 for clippy, on every source edit, for
      # byte-identical output. Dependencies are not instrumented — the
      # wrapper's `CRATE_NAMES` covers workspace crates only — so they are
      # reusable, just misplaced. Moving the tree to where the build looks is
      # the whole fix.
      #
      # The count is the check: `nix log` on this derivation should show ~11
      # `Compiling` lines, not 497. A silent return to 497 leaves the gate
      # green and slow, which is why the number is worth reading rather than
      # the clock.
      # Moved wholesale rather than glob-by-glob: the unpacked tree carries
      # dotfiles (`.rustc_info.json`, `.fingerprint`) that a bare `target/*`
      # would leave behind, and a half-moved cache is a silent partial rebuild.
      preBuild = ''
        mv target .llvm-cov-deps
        mkdir -p target
        mv .llvm-cov-deps target/llvm-cov-target
      '';
    }
  );

  # What the two suites below share: they each stand up an ephemeral
  # postgres+PostGIS cluster, which wants initdb/pg_ctl on PATH and a socket
  # directory short enough for a unix socket address (~104 bytes on darwin).
  #
  # The harness reaps its cluster with a watchdog process, and nix kills
  # everything the build started the moment the derivation ends, so that
  # watchdog never runs here; at its default base (/tmp) every build then left a
  # socket directory behind on the host. Pointing the base at the build
  # directory hands the lifetime to nix, which removes that tree whether the
  # build passed or failed, and puts the socket beside the PGDATA the harness
  # already keeps there.
  pgClusterArgs = {
    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ postgresWithPostgis ];
    preBuild = ''
      export PG_SOCKET_BASE="$NIX_BUILD_TOP"
    '';
  };

  # libtest otherwise takes a thread per core, and each test holds a pool
  # (`TEST_POOL_MAX_CONNECTIONS` = 5) against a cluster capped at 500 clients:
  # uncapped, a big enough builder reaches "sorry, too many clients already" and
  # the gate fails on the size of the machine. Sixteen tests in flight is 80
  # connections, and the cap binds only on machines larger than that.
  pgTestThreads = "--test-threads=16";

  # The db crate's postgres conformance suite over an ephemeral cluster.
  # Compile- and cluster-heavy, so it is let-bound like the other heavy checks
  # and ordered before `web-test` (see `CHRONOSCOPE_RUN_AFTER` there): left
  # co-schedulable, it would starve the headless-Chrome CDP event loop the same
  # way.
  #
  # `pname` is deliberately short: it lands in the build directory's name, which
  # the socket path now sits under.
  postgres-smoke = craneLib.cargoTest (
    checkArgs
    // testExtraEnv
    // pgClusterArgs
    // {
      pname = "pg-smoke";
      # No name filter: a `postgres::` prefix would silently exclude any future
      # feature-gated test placed outside that module path. Running the crate's
      # whole suite under the feature costs the sqlite cases a second run and
      # needs nobody to remember a convention.
      cargoTestExtraArgs = "-p chronoscope-db --features postgres -- ${pgTestThreads}";
    }
  );

  # The api's own integration suite against the fact-store backend production
  # runs. `clippy` already type-checks the server under `postgres` (it lints
  # `--all-features`); what this adds is *running* it against a live database:
  # the handlers, the listing, the cursors and the read views they open. A
  # serving path that compiles but does not work then fails a check rather than
  # a deploy.
  #
  # Under the feature the suite's store fixtures come from `chronoscope-db`'s
  # cluster harness (its `test-support` feature, a dev-dependency of this
  # crate), so each test drives real requests through the handlers against a
  # throwaway Postgres database.
  api-postgres = craneLib.cargoTest (
    checkArgs
    // testExtraEnv
    // pgClusterArgs
    // {
      pname = "api-pg";
      cargoTestExtraArgs = "-p chronoscope-api --features postgres -- ${pgTestThreads}";
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
      api-postgres
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
    # (including the two cluster-heavy Postgres suites) orders this derivation
    # after them, so the browser tests run unstarved; the reference only
    # establishes build order.
    web-test = craneLib.cargoTest (
      checkArgs
      // testExtraEnv
      // {
        pname = "chronoscope-web-tests";
        cargoTestExtraArgs = "-p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4";
        CHRONOSCOPE_RUN_AFTER = "${clippy} ${doc} ${doctest} ${llvm-cov} ${postgres-smoke} ${api-postgres}";
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
