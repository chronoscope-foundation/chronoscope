# Wikidata entity pipeline as Nix derivations.
#
# Three layers per bundle:
#   1. Entity fetch (FOD) — resolve + fetch entities at a pinned timestamp
#   2. Bundle creation    — pure transform: JSONL → IngestionBundle
#   3. Test database      — pure: load bundle into SQLite
#
# Bundle data is inline below. Each bundle is { timestamp, hash, entities }
# where entities maps Q-IDs to expected English labels. The fetch command
# validates labels against fetched data, so typos in Q-IDs are caught
# automatically. After changing entities or timestamp, rebuild with a dummy
# hash and paste the correct hash from the Nix error message.
{
  pkgs,
  lib,
  craneLib,
  rustCommonArgs,
}:

let
  # ======================== Bundle definitions ========================

  bundleDefs = {
    # Core test set: exercises different ingestion code paths and overlaps
    # with corpus images for future entity-analysis bridge testing.
    curated = {
      timestamp = "2022-01-03T00:00:00Z";
      hash = "sha256-crO3zJaJAkpjsmJ+CdQdpnFFCaS9y4wcUCFmWDDE9BU=";
      entities = {
        "Q243" = "Eiffel Tower";
        "Q2981" = "Notre-Dame de Paris";
        "Q12506" = "Hagia Sophia";
        "Q125006" = "Brooklyn Bridge";
        "Q1111481" = "Chioggia Cathedral";
        "Q4356655" = "Saint Thomas Church";
        "Q5171466" = "Cornelius Vanderbilt II House";
        "Q5652831" = "William K. Vanderbilt House";
        "Q108584685" = "Vanderbilt Triple Palace";
      };
    };
  };

  # ======================== Build infrastructure ========================

  # Build the ingest binary via crane.
  ingestBin = craneLib.buildPackage (
    rustCommonArgs
    // {
      pname = "ingest";
      cargoExtraArgs = "-p chronoscope-ingestion --bin ingest";
      cargoArtifacts = craneLib.buildDepsOnly (
        rustCommonArgs
        // {
          pname = "ingest-deps";
          cargoExtraArgs = "-p chronoscope-ingestion";
        }
      );
      doCheck = false;
    }
  );

  # Build the derivation chain for a single bundle.
  mkBundle =
    name: bundle:
    let
      # Format --entity Q=Name arguments from the entities attrset.
      entityArgs = lib.concatStringsSep " " (
        lib.mapAttrsToList (qid: label: "--entity ${lib.escapeShellArg "${qid}=${label}"}") bundle.entities
      );

      # Layer 1: FOD — fetch entities from Wikidata API.
      entities = pkgs.stdenvNoCC.mkDerivation {
        name = "chronoscope-wikidata-${name}-entities";
        outputHashMode = "recursive";
        outputHashAlgo = "sha256";
        outputHash = bundle.hash;
        nativeBuildInputs = [
          ingestBin
          pkgs.cacert
        ];
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        buildCommand = ''
          mkdir -p $out
          ingest fetch \
            --timestamp ${lib.escapeShellArg bundle.timestamp} \
            ${entityArgs} \
            --output $out/entities.jsonl
        '';
      };

      # Layer 2: Pure — transform JSONL to IngestionBundle + validate.
      ingestionBundle = pkgs.stdenvNoCC.mkDerivation {
        name = "chronoscope-wikidata-${name}-bundle";
        nativeBuildInputs = [ ingestBin ];
        buildCommand = ''
          mkdir -p $out
          ingest bundle \
            --input ${entities}/entities.jsonl \
            --output $out/bundle.json
          ingest check --input $out/bundle.json > $out/check-report.json
        '';
      };

      # Layer 3: Pure — load bundle into SQLite database.
      testDb = pkgs.stdenvNoCC.mkDerivation {
        name = "chronoscope-wikidata-${name}-db";
        nativeBuildInputs = [
          ingestBin
          pkgs.sqlite
        ];
        buildCommand = ''
          mkdir -p $out
          ingest load \
            --db $out/wikidata.db \
            --input ${ingestionBundle}/bundle.json
          # Checkpoint WAL into main DB file so the output is self-contained.
          sqlite3 $out/wikidata.db "PRAGMA wal_checkpoint(TRUNCATE);"
          rm -f $out/wikidata.db-wal $out/wikidata.db-shm
        '';
      };
    in
    {
      inherit entities ingestionBundle testDb;
    };

  # Build all bundles.
  bundles = lib.mapAttrs mkBundle bundleDefs;

in
{
  inherit bundles ingestBin;
}
