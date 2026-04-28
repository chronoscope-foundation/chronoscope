# Administrative region pipeline as Nix derivations.
#
# Two layers:
#   1. OSM PBF extract (FOD) → osmium filter (boundaries only)
#   2. region-builder — reads PBF via cosmogony library, writes SpatiaLite DB
#
# Start with Italy (~2 GB) for fast iteration; swap FOD for planet (~70 GB)
# when ready for production.
{
  pkgs,
  lib,
  craneLib,
  rustCommonArgs,
}:

let
  # ======================== Region builder ========================

  # Build region-builder from the workspace. It depends on cosmogony
  # (Rust crate) which requires GEOS at build time.
  regionBuilderExtraInputs = {
    nativeBuildInputs = (rustCommonArgs.nativeBuildInputs or [ ]) ++ [
      pkgs.pkg-config
    ];
    buildInputs =
      (rustCommonArgs.buildInputs or [ ])
      ++ [ pkgs.geos ]
      ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
  };

  regionBuilderBin = craneLib.buildPackage (
    rustCommonArgs
    // regionBuilderExtraInputs
    // {
      pname = "region-builder";
      cargoExtraArgs = "-p region-builder";
      cargoArtifacts = craneLib.buildDepsOnly (
        rustCommonArgs
        // regionBuilderExtraInputs
        // {
          pname = "region-builder-deps";
          cargoExtraArgs = "-p region-builder";
        }
      );
      doCheck = false;
    }
  );

  # ======================== OSM PBF extracts (FODs) ========================

  # Italy extract for development iteration (~2 GB). Default for `api-italy`
  # and the dev shell. To bump: change the date, set hash to lib.fakeHash,
  # rebuild — Nix reports the correct hash.
  italyPbf = pkgs.fetchurl {
    url = "https://download.geofabrik.de/europe/italy-260425.osm.pbf";
    hash = "sha256-54WU7TsePWN0A0XNiwiP8frNJ3Vcv7bfZk7qm1dyD7I=";
  };

  # Full planet PBF for production (~86 GB). Powers `api-world`.
  planetPbf = pkgs.fetchurl {
    url = "https://planet.openstreetmap.org/pbf/planet-260420.osm.pbf";
    hash = "sha256-JQLKwBNSd+GPvepmKnYArCQNYwbUrXwnz477o+2AN88=";
  };

  # ======================== Derivation chain ========================

  mkRegions =
    name: pbf:
    let
      # Layer 1: osmium filter — extract only boundary=administrative relations.
      # Shrinks input to ~1-2% of original size for faster cosmogony processing.
      boundariesPbf = pkgs.stdenvNoCC.mkDerivation {
        name = "chronoscope-regions-${name}-boundaries";
        nativeBuildInputs = [ pkgs.osmium-tool ];
        buildCommand = ''
          osmium tags-filter ${pbf} \
            r/boundary=administrative \
            -o boundaries.osm.pbf
          mkdir -p $out
          mv boundaries.osm.pbf $out/
        '';
      };

      # Layer 2: region-builder — reads PBF via cosmogony library, writes SpatiaLite.
      # Cosmogony extracts admin zones, then we insert into SpatiaLite with R-tree indexes.
      db = pkgs.stdenvNoCC.mkDerivation {
        name = "chronoscope-regions-${name}-db";
        nativeBuildInputs = [
          regionBuilderBin
          pkgs.sqlite
        ];
        SPATIALITE_LIBRARY_PATH = "${pkgs.libspatialite}/lib";
        buildCommand = ''
          mkdir -p $out
          region-builder \
            --input ${boundariesPbf}/boundaries.osm.pbf \
            --output $out/regions.sqlite
          # Checkpoint WAL and switch to DELETE journal mode so the DB can
          # be opened read-only from the Nix store.
          sqlite3 $out/regions.sqlite "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;"
          rm -f $out/regions.sqlite-wal $out/regions.sqlite-shm
        '';
      };
    in
    {
      inherit name boundariesPbf db;
    };

  italy = mkRegions "italy" italyPbf;
  world = mkRegions "world" planetPbf;

in
{
  inherit regionBuilderBin;

  regions = {
    inherit italy world;
  };
}
