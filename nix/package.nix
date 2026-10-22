{
  lib,
  rustPlatform,
  pkg-config,
  cmake,
  perl,
  nixosTests ? null,
}:

let
  root = ../.;
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;
in
rustPlatform.buildRustPackage {
  pname = "oxicale";
  inherit version;

  src = lib.cleanSourceWith {
    src = root;
    filter =
      path: _type:
      let
        relative = lib.removePrefix (toString root + "/") (toString path);
        excluded =
          relative == "target"
          || lib.hasPrefix "target/" relative
          || relative == ".devenv"
          || lib.hasPrefix ".devenv/" relative
          || relative == ".git"
          || lib.hasPrefix ".git/" relative
          || relative == "radicale"
          || lib.hasPrefix "radicale/" relative
          || lib.hasPrefix "result" relative;
      in
      !excluded;
  };

  cargoLock.lockFile = root + "/Cargo.lock";
  cargoBuildFlags = [
    "--package"
    "oxicale"
  ];

  nativeBuildInputs = [
    pkg-config
    cmake
    perl
  ];

  # The workspace test suite is better run outside the Nix sandbox; some
  # subscription tests exercise DNS and can be slow. The flake exposes a
  # dedicated NixOS VM check instead.
  doCheck = false;

  passthru = lib.optionalAttrs (nixosTests != null) {
    tests = {
      inherit (nixosTests) oxicale;
    };
  };

  meta = {
    description = "Self-hosted CalDAV and CardDAV server";
    homepage = "https://example.invalid/oxicale";
    license = lib.licenses.gpl3Plus;
    mainProgram = "oxicale";
    platforms = lib.platforms.unix;
  };
}
