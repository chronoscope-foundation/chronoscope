{
  pkgs,
  craneLib,
  rustCommonArgs,
  cargoArtifacts,
}:

let
  apiBin = craneLib.buildPackage (
    rustCommonArgs
    // {
      inherit cargoArtifacts;
      pname = "chronoscope-api";
      cargoExtraArgs = "-p chronoscope-api --bin chronoscope-api";
      doCheck = false;
    }
  );

  mkApi =
    { regions }:
    pkgs.symlinkJoin {
      name = "chronoscope-api-${regions.name}";
      paths = [ apiBin ];
      nativeBuildInputs = [ pkgs.makeWrapper ];
      postBuild = ''
        wrapProgram $out/bin/chronoscope-api \
          --set REGIONS_DB "${regions.db}/regions.sqlite" \
          --set SPATIALITE_LIBRARY_PATH "${pkgs.libspatialite}/lib"
      '';
      meta = {
        description = "Chronoscope API server (regions: ${regions.name})";
      };
    };

in
{
  inherit mkApi;
}
