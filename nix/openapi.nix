# OpenAPI spec generation. The `openapi` binary registers the API and
# serializes the schema — pure, so it needs no DB/spatialite at runtime.
{
  pkgs,
  craneLib,
  rustCommonArgs,
  cargoArtifacts,
  iosApiSrc,
  iosProjectTemplate,
}:

let
  openapiBin = craneLib.buildPackage (
    rustCommonArgs
    // {
      inherit cargoArtifacts;
      pname = "chronoscope-openapi";
      cargoExtraArgs = "-p chronoscope-api --bin openapi";
      doCheck = false;
    }
  );

  spec = pkgs.runCommand "chronoscope-openapi-spec" { } ''
    mkdir -p $out
    ${openapiBin}/bin/openapi $out/openapi.json
  '';

  # The hand-written ChronoscopeAPI SwiftPM package with the generated spec
  # dropped in; the generated Xcode project consumes it from the store.
  apiPackage = pkgs.runCommand "chronoscope-ios-api-package" { } ''
    mkdir -p $out
    cp -r ${iosApiSrc}/. $out/
    chmod -R u+w $out
    cp ${spec}/openapi.json $out/Sources/ChronoscopeAPI/
  '';

  # The xcodegen spec with store paths spliced in for the API package and the
  # Swift tools. Pinning it keeps the whole closure (package + tools) alive.
  projectSpec = pkgs.replaceVars iosProjectTemplate {
    ios_api_package = "${apiPackage}";
    swiftformat = "${pkgs.swiftformat}";
    swiftlint = "${pkgs.swiftlint}";
  };
in
{
  inherit spec apiPackage projectSpec;
}
