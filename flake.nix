{
  description = "Oxicale CalDAV and CardDAV server";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          oxicale = pkgs.callPackage ./nix/package.nix { };
          default = self.packages.${system}.oxicale;
        }
      );

      nixosModules.oxicale = import ./nix/module.nix;
      nixosModules.default = self.nixosModules.oxicale;

      checks = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ] (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          oxicale = pkgs.testers.nixosTest (import ./nix/tests/oxicale.nix { inherit pkgs; });
        }
      );
    };
}
