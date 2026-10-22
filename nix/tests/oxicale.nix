{ pkgs, ... }:

let
  oxicale = pkgs.callPackage ../package.nix { };
in
{
  name = "oxicale";
  meta.maintainers = [ ];

  nodes.machine =
    { config, ... }:
    {
      imports = [ ../module.nix ];

      services.oxicale = {
        enable = true;
        package = oxicale;
        settings = {
          server.bind = [ "127.0.0.1:5232" ];
          server.external_url = "http://127.0.0.1:5232";
          storage.data_dir = "/var/lib/oxicale";
          auth = {
            backend = "local";
            password_file = "/var/lib/oxicale/password";
          };
        };
      };
    };

  testScript = ''
    machine.wait_for_unit("oxicale.service")
    machine.wait_for_open_port(5232)

    machine.succeed("curl --fail http://127.0.0.1:5232/healthz")
    machine.succeed("curl --fail http://127.0.0.1:5232/readyz")

    machine.succeed(
        "sudo -u oxicale ${oxicale}/bin/oxicale user add alice "
        "--password-file /var/lib/oxicale/password --password secret"
    )
    machine.succeed(
        "curl --fail -u alice:secret -X PROPFIND -H 'Depth: 0' "
        "http://127.0.0.1:5232/ | grep -q current-user-principal"
    )
    machine.succeed(
        "curl --fail -u alice:secret -X PROPFIND -H 'Depth: 1' "
        "http://127.0.0.1:5232/alice/ | grep -q /alice/calendar/"
    )
  '';
}
