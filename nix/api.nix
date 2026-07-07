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

  api = pkgs.symlinkJoin {
    name = "chronoscope-api";
    paths = [ apiBin ];
    nativeBuildInputs = [ pkgs.makeWrapper ];
    postBuild = ''
      wrapProgram $out/bin/chronoscope-api \
        --set SPATIALITE_LIBRARY_PATH "${pkgs.libspatialite}/lib"
    '';
    meta = {
      description = "Chronoscope API server";
    };
  };

in
{
  inherit api;
}
