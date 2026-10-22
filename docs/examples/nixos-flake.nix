# Complete flake example for a NixOS host running Oxicale behind a local
# reverse proxy. Replace OWNER with the repository owner and adjust the host
# paths/domain.
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    oxicale.url = "github:OWNER/oxicale";
    sops-nix.url = "github:Mic92/sops-nix";
  };

  outputs = { self, nixpkgs, oxicale, sops-nix, ... }: {
    nixosConfigurations.calendar = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./hardware-configuration.nix
        sops-nix.nixosModules.sops
        oxicale.nixosModules.default
        ({ config, ... }: {
          sops.secrets.oxicale-users = {
            owner = config.services.oxicale.user;
            mode = "0400";
          };

          services.oxicale = {
            enable = true;
            settings = {
              server = {
                bind = [ "127.0.0.1:5232" ];
                external_url = "https://calendar.example.com";
                behind_proxy = true;
                trusted_proxies = [ "127.0.0.1/32" ];
              };
              auth = {
                backend = "local";
                password_file = config.sops.secrets.oxicale-users.path;
              };
              web.require_https = true;
            };
          };

          # Keep Oxicale on loopback and terminate TLS in your reverse proxy.
          services.nginx = {
            enable = true;
            recommendedProxySettings = true;
            virtualHosts."calendar.example.com" = {
              enableACME = true;
              forceSSL = true;
              locations."/".proxyPass = "http://127.0.0.1:5232";
            };
          };
        })
      ];
    };
  };
}
