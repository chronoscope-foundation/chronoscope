# Container image for the API server, shaped for Cloud Run.
#
# nix2container rather than dockerTools: the output is a manifest that points
# at store paths, so a build costs a JSON file instead of a fresh
# multi-hundred-megabyte tarball, and a push moves only the layers the registry
# is missing.
{
  pkgs,
  lib,
  nix2container,
  apiBin,
  runtimeEnv,
  factsDb,
}:

let
  # Cloud Run runs whatever uid the config names; a non-root one keeps a
  # compromised handler off the container's own filesystem.
  serviceUser = "chronoscope";
  serviceUid = 65532;

  # The app DB URL defaults to a *relative* sqlite path, so the working
  # directory has to be somewhere the service user can write. On Cloud Run that
  # instance-local DB is ephemeral wherever it lands, so /tmp is the honest
  # home for it.
  workingDir = "/tmp";

  factsDbPath = "${factsDb}/facts.db";

  passwd = pkgs.writeText "passwd" ''
    root:x:0:0:root:/:/sbin/nologin
    ${serviceUser}:x:${toString serviceUid}:${toString serviceUid}:${serviceUser}:/:/sbin/nologin
  '';

  group = pkgs.writeText "group" ''
    root:x:0:
    ${serviceUser}:x:${toString serviceUid}:
  '';

  # What an otherwise-empty image lacks and this server needs at boot: a
  # writable /tmp (the facts overlay is a TempDir minted during startup), a CA
  # bundle under both names TLS stacks look for, and a passwd/group pair so the
  # uid in the image config resolves to a name.
  rootfs = pkgs.runCommand "chronoscope-api-rootfs" { } ''
    mkdir -p $out/tmp $out/etc/ssl/certs
    cp ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt $out/etc/ssl/certs/ca-certificates.crt
    ln -s ca-certificates.crt $out/etc/ssl/certs/ca-bundle.crt
    cp ${passwd} $out/etc/passwd
    cp ${group} $out/etc/group
  '';

  # The C runtime the server stands on: what the binary links (glibc, openssl,
  # sqlite) plus libspatialite and its geo closure, which arrives by dlopen
  # through SPATIALITE_LIBRARY_PATH and so is absent from the binary's own
  # references. `ignore` drops the binary itself, leaving exactly the part that
  # turns over with nixpkgs rather than with Chronoscope. maxLayers spreads it
  # by store-path popularity so a nixpkgs bump re-pushes the libraries that
  # moved, not all of them.
  runtimeLayer = nix2container.buildLayer {
    deps = [
      apiBin
      pkgs.libspatialite
    ];
    ignore = apiBin;
    maxLayers = 16;
  };

  # The baked read-only facts base. Turns over on a data refresh, which is a
  # different clock from either the C stack below it or the binary above.
  factsLayer = nix2container.buildLayer {
    deps = [ factsDb ];
    layers = [ runtimeLayer ];
  };

  rootfsLayer = nix2container.buildLayer {
    copyToRoot = [ rootfs ];
    perms = [
      # /tmp is shared scratch for whatever the image ends up running, so it
      # carries the sticky world-writable mode /tmp has everywhere else.
      {
        path = rootfs;
        regex = "/tmp$";
        mode = "1777";
      }
      # Store directories arrive 0555. A container runtime materializes
      # /etc/resolv.conf and /etc/hosts into the image's /etc during setup, and
      # the DNS resolver reads the former at startup, so /etc keeps the owner
      # write bit that lets that setup land.
      {
        path = rootfs;
        regex = "/etc$";
        mode = "0755";
      }
    ];
    layers = [
      runtimeLayer
      factsLayer
    ];
  };

  # `packages.api` wraps this same binary in a script whose one job is to export
  # the server's runtime environment before exec'ing. An image config carries
  # environment natively, so here the binary itself is pid 1 and the same
  # attrset lands as OCI Env.
  imageEnv = runtimeEnv // {
    CHRONOSCOPE_FACTS_DB = factsDbPath;
    SSL_CERT_FILE = "/etc/ssl/certs/ca-certificates.crt";
  };

  image = nix2container.buildImage {
    name = "chronoscope-api";
    # Every layer the image config's closure would otherwise sweep into the top
    # layer is named here, so what a code change pushes is the binary alone.
    layers = [
      runtimeLayer
      factsLayer
      rootfsLayer
    ];

    config = {
      # Entrypoint rather than Cmd: the server takes its whole configuration
      # from the environment, so a platform that appends container arguments
      # should leave the program it appends them to alone.
      Entrypoint = [ "${apiBin}/bin/chronoscope-api" ];
      Env = lib.mapAttrsToList (name: value: "${name}=${value}") imageEnv;
      # Numeric, because a runtime that enforces "must not run as root" reads
      # the config rather than the image's /etc/passwd. The passwd entry above
      # is what makes the uid resolve to a name once inside.
      User = "${toString serviceUid}:${toString serviceUid}";
      WorkingDir = workingDir;
      # Matches the server's default BIND_ADDR. Cloud Run injects its own PORT
      # and the server follows it, so this is documentation for every other
      # runtime.
      ExposedPorts = {
        "8080/tcp" = { };
      };
    };

    meta = {
      description = "Chronoscope API server container image";
      platforms = lib.platforms.linux;
    };
  };

  # An image that builds proves the manifest assembles, which says nothing
  # about whether the process inside it starts: a binary with no RPATH built a
  # perfectly good image and died at exec. So run the entrypoint under the
  # image's own environment and hold it to getting past the parts a broken
  # image fails at. Withholding JWT_SECRET is what makes that deterministic and
  # cheap: with everything else supplied, the first thing the server cannot do
  # is read it, so `SecretNotConfigured` on stderr places the process past the
  # dynamic loader, past its configuration, and past opening a SpatiaLite-loaded
  # pool. A loader failure exits 127 with none of that.
  boots =
    pkgs.runCommand "chronoscope-api-boots"
      {
        env = imageEnv;
        meta.platforms = lib.platforms.linux;
      }
      ''
        entrypoint=${apiBin}/bin/chronoscope-api
        grep -q "$entrypoint" ${image} \
          || { echo "image entrypoint is not the binary under test" >&2; exit 1; }

        status=0
        DATABASE_URL="sqlite:$PWD/app.db" "$entrypoint" > boot.log 2>&1 || status=$?

        if [ "$status" -ne 1 ]; then
          echo "expected exit 1 from the server's own error path, got $status" >&2
          cat boot.log >&2
          exit 1
        fi
        for marker in "Starting Chronoscope API server" SecretNotConfigured; do
          grep -q "$marker" boot.log \
            || { echo "server never reached: $marker" >&2; cat boot.log >&2; exit 1; }
        done

        touch $out
      '';

in
{
  inherit image boots;
}
