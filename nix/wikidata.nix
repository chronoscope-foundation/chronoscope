# Wikidata entity pipeline as Nix derivations.
#
# One layer per bundle: an entity fetch (FOD) that resolves + fetches
# entities at a pinned timestamp, producing entities.jsonl.
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
  cargoArtifacts,
}:

let
  # ======================== Bundle definitions ========================

  bundleDefs = {
    # Core test set: exercises different ingestion code paths and overlaps
    # with corpus images for future entity-analysis bridge testing.
    #
    # Italian entities are organized for region-clustering tests:
    #   - Rome has 4 entities (city-level multi-entity clustering)
    #   - Lombardy, Veneto, and Apulia each have 2 entities in different
    #     cities (region-level clustering with count > 1)
    #   - 8 distinct Italian regions total (state-level clustering)
    curated = {
      timestamp = "2022-01-03T00:00:00Z";
      hash = "sha256-Q+YS0e4ZRnL+ivIBaGh5R8gZUX6nWeQPbZ0zGcV/+7E=";
      entities = {
        # Original test set (non-Italian + Chioggia)
        "Q243" = "Eiffel Tower";
        "Q2981" = "Notre-Dame de Paris";
        "Q12506" = "Hagia Sophia";
        "Q125006" = "Brooklyn Bridge";
        "Q1111481" = "Chioggia Cathedral";
        "Q4356655" = "Saint Thomas Church";
        "Q5171466" = "Cornelius Vanderbilt II House";
        "Q5652831" = "William K. Vanderbilt House";
        "Q108584685" = "Vanderbilt Triple Palace";

        # Usage-transition regression set: an entity's repeated openings must
        # project as distinct events, not collapse into one conflicting event.
        # BER opened once behind a long trail of deprecated planned dates;
        # Bostancı station opened across several distinct years.
        "Q160556" = "Berlin Brandenburg Airport";
        "Q4947652" = "Bostancı railway station";

        # Rome (Lazio) — 4 entities for city-level multi-entity tests
        "Q192784" = "Trajan's Column";
        "Q10285" = "Colosseum";
        "Q99309" = "Pantheon";
        "Q486382" = "Castel Sant'Angelo";

        # Lombardy — 2 cities (Bellagio, Certosa di Pavia)
        "Q650088" = "Villa Melzi d'Eril";
        "Q654443" = "Certosa di Pavia";

        # Apulia — 2 buildings (Andria, Alberobello)
        "Q215897" = "Castel del Monte";
        "Q1324513" = "Trullo Sovrano";

        # Single-entity regions
        "Q201902" = "Mole Antonelliana"; # Piedmont / Turin
        "Q189883" = "Doge's Palace"; # Veneto / Venice
        "Q208633" = "Ponte Vecchio"; # Tuscany / Florence
        "Q1799127" = "La Scarzuola"; # Umbria / Montegabbione
      };
    };
  };

  # ======================== Build infrastructure ========================

  # Build the ingest binary via crane.
  ingestBin = craneLib.buildPackage (
    rustCommonArgs
    // {
      inherit cargoArtifacts;
      pname = "ingest";
      cargoExtraArgs = "-p chronoscope-ingestion --bin ingest";
      doCheck = false;
    }
  );

  # Build the derivation for a single bundle.
  mkBundle =
    name: bundle:
    let
      # Format --entity Q=Name arguments from the entities attrset.
      entityArgs = lib.concatStringsSep " " (
        lib.mapAttrsToList (qid: label: "--entity ${lib.escapeShellArg "${qid}=${label}"}") bundle.entities
      );

      # FOD — fetch entities from Wikidata API.
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
    in
    {
      inherit entities;
    };

  # Build all bundles.
  bundles = lib.mapAttrs mkBundle bundleDefs;

  # ======================== Full-dump pipeline ========================
  #
  # The bulk path: fetch the whole Wikidata JSON dump (FOD), derive the
  # architectural type set from its own P279 graph, and filter down to
  # architectural entities. The resolve/filter steps are pure — they re-run
  # only when the dump, the ingest binary, or the type set changes, so the
  # expensive work happens rarely. None of these are flake `checks`, so the
  # commit gate never touches the multi-gigabyte dump.

  dumpPin = builtins.fromJSON (builtins.readFile ./wikidata-dump.json);

  # The academic-torrents tracker holds the seeders; public trackers + DHT
  # widen discovery. A bare infohash magnet reaches none of these on its own.
  dumpTrackers = [
    "https://academictorrents.com/announce.php"
    "udp://tracker.opentrackr.org:1337/announce"
    "udp://open.stealth.si:80/announce"
    "udp://exodus.desync.com:6969/announce"
  ];

  # FOD: the full dump over BitTorrent. Hand-rolled on aria2 rather than
  # pkgs.fetchtorrent because the dump is a single-file torrent, and
  # fetchtorrent's flatten step (mv $dir/*/*) assumes a multi-file layout.
  dumpFull = pkgs.stdenvNoCC.mkDerivation {
    name = "wikidata-dump-${dumpPin.date}";
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = dumpPin.hash;
    nativeBuildInputs = [
      pkgs.aria2
      pkgs.cacert
    ];
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    buildCommand = ''
      mkdir -p $out
      aria2c \
        --dir=$out \
        --seed-time=0 \
        --file-allocation=none \
        --check-integrity=true \
        --bt-tracker=${lib.concatStringsSep "," dumpTrackers} \
        ${lib.escapeShellArg dumpPin.magnet}
      test -f "$out/${dumpPin.filename}"
    '';
  };

  # The dump file inside the FOD output.
  dumpFile = "${dumpFull}/${dumpPin.filename}";

  # Pure: architectural type set resolved from the dump's own P279 subclass
  # graph. The class hierarchy tracks the dump's snapshot, not live Wikidata.
  wikidataArchTypes = pkgs.stdenvNoCC.mkDerivation {
    name = "wikidata-arch-types-${dumpPin.date}";
    nativeBuildInputs = [ ingestBin ];
    buildCommand = ''
      mkdir -p $out
      ingest resolve-types --input ${dumpFile} --output $out/arch-types.json --verbose
    '';
  };

  # Pure: the architectural entities as JSONL — the durable artifact worth
  # pinning, orders of magnitude smaller than the dump it came from.
  wikidataArchEntities = pkgs.stdenvNoCC.mkDerivation {
    name = "wikidata-arch-entities-${dumpPin.date}";
    nativeBuildInputs = [ ingestBin ];
    buildCommand = ''
      mkdir -p $out
      ingest filter \
        --input ${dumpFile} \
        --types ${wikidataArchTypes}/arch-types.json \
        --output $out/entities.jsonl \
        --verbose
    '';
  };

in
{
  inherit bundles ingestBin;
  dump = {
    full = dumpFull;
    archTypes = wikidataArchTypes;
    archEntities = wikidataArchEntities;
  };
}
