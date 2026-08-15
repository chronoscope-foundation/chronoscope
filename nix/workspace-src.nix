# Sources for artifacts built out of one crate of the workspace.
#
# A cargo workspace has one source tree, but a build that names a single
# package compiles only that package's closure. Handing it the whole tree
# anyway puts every other crate in the derivation's input hash, so editing an
# unrelated crate produces a different output path: a new container image, a
# republished bundle, a rebuild of a pipeline that cannot have changed.
#
# The fileset helpers come from crane, which supplies the primitives but not
# the closure. Deriving that from cargo would mean import-from-derivation, so
# the closure is declared and `mkClosureCheck` keeps the declaration honest.
{
  lib,
  pkgs,
  craneLib,
}:

let
  closureScript = pkgs.writeText "check-workspace-closures.py" ''
    """Compare each declared crate list against the closure cargo reports."""

    import json
    import os
    import sys

    meta = json.load(open(sys.argv[1]))
    expected = json.load(open(sys.argv[2]))

    # Cargo names packages; deployable crate lists name directories relative to
    # the workspace root; manifest_path relates the two. Root-relative, not the
    # basename, so a crate nested under a grouping dir (tools/quantize) maps to
    # its real path. Top-level crates are unaffected: their relpath is the base.
    members = {p["name"]: p for p in meta["packages"]}
    root = meta["workspace_root"]
    directory = {
        name: os.path.relpath(os.path.dirname(p["manifest_path"]), root)
        for name, p in members.items()
    }


    def closure(root):
        seen, stack = set(), [root]
        while stack:
            name = stack.pop()
            if name in seen:
                continue
            seen.add(name)
            for dep in members[name]["dependencies"]:
                if dep["name"] in members:
                    stack.append(dep["name"])
        return seen


    failures = []
    for key, spec in sorted(expected.items()):
        if spec["package"] not in members:
            failures.append(f"{key}: no workspace member named {spec['package']}")
            continue
        actual = {directory[n] for n in closure(spec["package"])}
        declared = set(spec["crates"])
        for missing in sorted(actual - declared):
            failures.append(
                f"{key}: {spec['package']} compiles '{missing}', which is not in "
                f"its crates list. Add it, or the build loses those sources."
            )
        for extra in sorted(declared - actual):
            failures.append(
                f"{key}: '{extra}' is listed but {spec['package']} does not compile "
                f"it. Remove it so editing that crate stops rebuilding this."
            )

    if failures:
        print("declared workspace closures are out of date:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print("\nThe lists live in `deployables` in flake.nix.", file=sys.stderr)
        sys.exit(1)

    print("closures match cargo: " + ", ".join(sorted(expected)))
  '';
in

{
  # The crates in `crates` from their real sources, every other member from a
  # stub, so what a build reads is only what it compiles.
  #
  # The stubs are not optional. Cargo loads every workspace member, and loading
  # one resolves its path dependencies' manifests, each of which must declare a
  # target. Dropping a crate's sources therefore breaks any *other* member that
  # depends on it, whether or not this build compiles either: leaving out
  # `workers` stops `dev` from loading, and the workspace never opens.
  #
  # `mkDummySrc` is crane's answer to the same problem for dependency-only
  # builds. It rewrites manifests at evaluation time and strips Nix's string
  # context as it goes, so the stub tree does not carry a dependency on the
  # sources it was derived from, and an edit anywhere leaves it untouched.
  #
  # `crates` is an allow-list rather than a deny-list because of which way each
  # one fails. A crate missing from an allow-list has no real sources and the
  # build stops; an unrelated crate missing from a deny-list quietly rejoins the
  # input hash, and the churn the list exists to prevent comes back with nothing
  # to signal it.
  mkWorkspaceSrc =
    {
      name,
      root,
      fullSrc,
      crates,
      extra ? [ ],
    }:
    let
      compiled = lib.fileset.toSource {
        inherit root;
        fileset = lib.fileset.unions (
          [ (craneLib.fileset.cargoTomlAndLock root) ]
          ++ map (crate: craneLib.fileset.commonCargoSources (root + "/${crate}")) crates
          ++ extra
        );
      };
      stubs = craneLib.mkDummySrc { src = fullSrc; };
    in
    pkgs.runCommand name { } ''
      # Both inputs are store paths, so every file and directory in them is
      # read-only. tar is what moves them rather than cp: `--no-same-permissions`
      # applies the umask instead of the archived mode, so the tree stays
      # writable and the overlay below can land on it. cp's --no-preserve=mode
      # does not manage that for the directories it creates along the way, which
      # cost a deploy: it failed on Linux having passed the darwin gate.
      mkdir -p "$out"
      tar -C ${stubs} -cf - . | tar -C "$out" -xf - --no-same-permissions --no-same-owner
      chmod -R u+w "$out"

      # A compiled crate comes entirely from its real sources. Landing them on
      # top of the stub would leave its dummy targets behind, so a crate with
      # only a main.rs would acquire a lib.rs it never had.
      ${lib.concatMapStringsSep "\n" (crate: "rm -rf \"$out/${crate}\"") crates}

      tar -C ${compiled} -cf - . | tar -C "$out" -xf - --no-same-permissions --no-same-owner
    '';

  # Files carrying any of `exts`, anywhere under `dir`. For the inputs a build
  # reads that are not cargo sources: embedded migrations, .proto compiled by a
  # build script, a stylesheet a bundler consumes.
  withExtensions = exts: dir: lib.fileset.fileFilter (file: lib.any (ext: file.hasExt ext) exts) dir;

  # Fails when a declared closure stops matching cargo's. The lists are written
  # by hand, and cargo's own response to a wrong one is a warning it prints
  # while dropping the dependency, so a mistake can reach a build as a
  # differently-linked binary rather than as an error. This turns that into a
  # gate failure naming the crate to add or remove.
  #
  # `--no-deps` reports each member's dependencies without resolving the
  # registry, so this needs no network and no vendored crates. Every dependency
  # kind counts: a dev-dependency is still a source that the test builds
  # sharing this source will compile.
  mkClosureCheck =
    {
      src,
      deployables,
      cargo,
    }:
    pkgs.runCommand "workspace-closures"
      {
        nativeBuildInputs = [
          cargo
          pkgs.python3
        ];
        expected = builtins.toJSON (
          lib.mapAttrs (_: deployable: {
            inherit (deployable) package crates;
          }) deployables
        );
        passAsFile = [ "expected" ];
      }
      ''
        export CARGO_HOME="$TMPDIR/cargo"
        cargo metadata --no-deps --offline --format-version 1 \
          --manifest-path ${src}/Cargo.toml > "$TMPDIR/meta.json"
        python3 ${closureScript} "$TMPDIR/meta.json" "$expectedPath"
        touch $out
      '';
}
