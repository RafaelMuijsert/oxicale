# NixOS Integration

Status: initial module and package.

Oxicale exposes a NixOS module as `services.oxicale`. The module and package are
currently developed in this repository and are intended to be submitted to
nixpkgs after release metadata is finalised.

## Examples

- [Complete flake example](examples/nixos-flake.nix)
- [den.ful + sops example](examples/den-ful-sops.nix)

## Flake usage

Add the repository as an input and import the module:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    oxicale.url = "github:OWNER/oxicale";
  };

  outputs = { self, nixpkgs, oxicale, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        oxicale.nixosModules.default
        {
          services.oxicale.enable = true;
        }
      ];
    };
  };
}
```

The default configuration listens on `127.0.0.1:5232`, stores data in
`/var/lib/oxicale`, and uses the local password backend with
`/var/lib/oxicale/password`.

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `enable` | `false` | Enable the service |
| `package` | local package | Oxicale package to use |
| `user` | `oxicale` | Service user |
| `group` | `oxicale` | Service group |
| `dataDir` | `/var/lib/oxicale` | Data directory when `settings.storage.data_dir` is unset |
| `passwordFile` | `{dataDir}/password` | Argon2 password file when `settings.auth.password_file` is unset |
| `secretKeyFile` | `{dataDir}/.oxicale.key` | Subscription credential key |
| `settings` | `{ }` | Full Oxicale TOML configuration in Nix form |
| `extraArgs` | `[ ]` | Extra arguments to `oxicale run` |
| `environmentFile` | `null` | Optional environment file, e.g. for `OXICALE_SECRET_KEY` |
| `openFirewall` | `false` | Open the main HTTP port |
| `firewallPort` | `5232` | Port opened when `openFirewall = true` |

`settings` is the full configuration surface. Examples:

```nix
services.oxicale.settings = {
  server = {
    bind = [ "127.0.0.1:5232" ];
    external_url = "https://calendar.example.com";
  };
  calendar.default_timezone = "Europe/Berlin";
  subscriptions.enable = true;
  feeds.enable = true;
  limits.max_collections_per_user = 100;
};
```

Durations are strings such as `"15m"`, and sizes are strings such as
`"10MiB"`.

## sops and password files

Oxicale's local backend requires Argon2id hashes, not Radicale's htpasswd
`plain` format. Generate a password file with:

```bash
sudo -u oxicale oxicale user add alice \
  --password-file /var/lib/oxicale/password
```

Store that file in sops and point the module at the secret path:

```nix
{
  sops.secrets.oxicale-users = {
    owner = config.services.oxicale.user;
    mode = "0400";
  };

  services.oxicale = {
    enable = true;
    settings = {
      server.bind = [ "127.0.0.1:5232" ];
      server.external_url = "https://calendar.example.com";
      auth = {
        backend = "local";
        password_file = config.sops.secrets.oxicale-users.path;
      };
    };
  };
}
```

The file must remain readable by the service user. It is never placed in the
Nix store by this module.

If your configuration uses a wrapper such as `den.ful` plus an internal
web-service abstraction, the equivalent shape is:

```nix
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
        server.bind = [ "127.0.0.1:${toString port}" ];
        server.external_url = "https://calendar.example.com";
        auth = {
          backend = "local";
          password_file = config.sops.secrets.oxicale-users.path;
        };
      };
    };
  };
};
```

The secret content must be an Oxicale Argon2 password file, not Radicale's
plaintext htpasswd file.

## Reverse proxy

Recommended setup: keep Oxicale on loopback and terminate TLS in nginx, Caddy or
your existing internal web-service abstraction.

```nix
services.oxicale.settings = {
  server = {
    bind = [ "127.0.0.1:5232" ];
    external_url = "https://calendar.example.com";
    behind_proxy = true;
    trusted_proxies = [ "127.0.0.1/32" ];
  };
  web.require_https = true;
};
```

The proxy must overwrite `X-Forwarded-Proto` and must be the only client allowed
to reach the Oxicale port.

## First login

`services.oxicale.enable = true` provisions no credentials. It starts the
server, creates the data directory and principal data on first authenticated
use, but an account must already exist in the password file or in the selected
external auth backend. Use `oxicale user add` or a sops-managed password file.

## Direct import without flakes

```nix
{ config, pkgs, ... }:
{
  imports = [ /path/to/oxicale/nix/module.nix ];
  services.oxicale = {
    enable = true;
    package = pkgs.callPackage /path/to/oxicale/nix/package.nix { };
  };
}
```

## Upstream nixpkgs

The intended upstream layout is:

```text
pkgs/by-name/ox/oxicale/package.nix
nixos/modules/services/networking/oxicale.nix
nixos/tests/oxicale.nix
```

Before upstreaming, the repository needs a real homepage URL, final license
confirmation, maintainer metadata and a tagged release that the package can
fetch.

## Local checkout note

Nix flake path inputs only respect `.gitignore` when the checkout is a Git
repository. In a plain directory with a populated `target/` or `.devenv/`,
`nix flake` may try to copy those directories. For local development either
initialize Git (`git init`) so `.gitignore` is honoured, or use the published
flake from its repository URL.

