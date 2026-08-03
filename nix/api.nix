{
  pkgs,
  craneLib,
  rustCommonArgs,
  cargoArtifacts,
  # Workspace source narrowed to the crates this binary compiles — see
  # flake.nix. Keeps an edit elsewhere in the workspace from producing a new
  # image digest and rolling a revision that cannot behave differently.
  apiSrc,
  # Env that preloads libspatialite to avoid the teardown segfault — see
  # flake.nix. `{}` keeps the module loadable standalone.
  spatialitePreload ? { },
}:

let
  # Everything the server needs in its environment to run at all. One
  # definition, because each way of launching it (wrapper script, dev shell,
  # OCI image config) has to carry the same set or the ones that miss an
  # addition fail at runtime.
  runtimeEnv = {
    SPATIALITE_LIBRARY_PATH = "${pkgs.libspatialite}/lib";
  }
  // spatialitePreload;

  apiBin = craneLib.buildPackage (
    rustCommonArgs
    // {
      inherit cargoArtifacts;
      src = apiSrc;
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
      wrapProgram $out/bin/chronoscope-api ${
        pkgs.lib.concatStringsSep " " (pkgs.lib.mapAttrsToList (n: v: ''--set ${n} "${v}"'') runtimeEnv)
      }
    '';
    meta = {
      description = "Chronoscope API server";
    };
  };

in
{
  # `api` is the shell/CLI entry point: a wrapper that exports `runtimeEnv`
  # before exec'ing. `apiBin` is the same binary with nothing in front of it,
  # for callers that can set the environment themselves: the container image
  # puts `runtimeEnv` in the OCI config, so the bash hop buys nothing there.
  inherit api apiBin runtimeEnv;
}
