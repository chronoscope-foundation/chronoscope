{
  pkgs,
  craneLib,
  rustCommonArgs,
  cargoArtifacts,
  # Env that preloads libspatialite to avoid the teardown segfault — see
  # flake.nix. `{}` keeps the module loadable standalone.
  spatialitePreload ? { },
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
        --set SPATIALITE_LIBRARY_PATH "${pkgs.libspatialite}/lib" ${
          pkgs.lib.concatStringsSep " " (
            pkgs.lib.mapAttrsToList (n: v: ''--set ${n} "${v}"'') spatialitePreload
          )
        }
    '';
    meta = {
      description = "Chronoscope API server";
    };
  };

in
{
  inherit api;
}
