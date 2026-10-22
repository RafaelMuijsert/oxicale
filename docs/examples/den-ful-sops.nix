# Example adapted to a den.ful-style service wrapper with sops secrets and an
# internal reverse proxy. `name`, `port` and `<sops>` come from the surrounding
# framework.
let
  name = "calendar";
  port = 8392;
in
{
  den.ful.services.${name} = {
    includes = [ <sops> ];
    webServices.internal.${name}.port = port;

    nixos = { config, ... }: {
      sops.secrets.oxicale-users = {
        owner = config.services.oxicale.user;
        mode = "0400";
      };

      services.oxicale = {
        enable = true;
        settings = {
          server = {
            bind = [ "127.0.0.1:${toString port}" ];
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
    };
  };
}
