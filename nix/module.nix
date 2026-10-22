{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.oxicale;
  toml = pkgs.formats.toml { };

  defaultDataDir = "/var/lib/oxicale";

  pkg =
    if pkgs ? oxicale then
      pkgs.oxicale
    else
      pkgs.callPackage ./package.nix { };

  configuredDataDir =
    if (cfg.settings.storage or { }) ? data_dir then
      cfg.settings.storage.data_dir
    else if cfg.dataDir != null then
      cfg.dataDir
    else
      defaultDataDir;

  dataDir = toString configuredDataDir;

  passwordFile =
    if (cfg.settings.auth or { }) ? password_file then
      cfg.settings.auth.password_file
    else if cfg.passwordFile != null then
      cfg.passwordFile
    else
      "${dataDir}/password";

  secretKeyFile =
    if (cfg.settings.storage or { }) ? secret_key_file then
      cfg.settings.storage.secret_key_file
    else if cfg.secretKeyFile != null then
      cfg.secretKeyFile
    else
      "${dataDir}/.oxicale.key";

  effectiveSettings = lib.recursiveUpdate {
    server.bind = [ "127.0.0.1:5232" ];
    storage.data_dir = dataDir;
    storage.secret_key_file = secretKeyFile;
    auth.backend = "local";
    auth.password_file = passwordFile;
  } cfg.settings;

  configFile = toml.generate "oxicale.toml" effectiveSettings;

  extraArgs = map lib.escapeShellArg cfg.extraArgs;

  usesDefaultStateDir = dataDir == defaultDataDir;
in
{
  options.services.oxicale = {
    enable = lib.mkEnableOption "Oxicale CalDAV and CardDAV server";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkg;
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "Oxicale package to use.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "oxicale";
      description = "User account under which Oxicale runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "oxicale";
      description = "Group under which Oxicale runs.";
    };

    dataDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Data directory used when `settings.storage.data_dir` is not set.
        Defaults to `/var/lib/oxicale`.
      '';
    };

    passwordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Password file used when `settings.auth.password_file` is not set.
        Defaults to `{dataDir}/password`. The file must contain Argon2id hashes,
        one `username:hash` per line.
      '';
    };

    secretKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Subscription credential encryption key file. Defaults to
        `{dataDir}/.oxicale.key` and is generated on first start if missing.
      '';
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      description = ''
        Oxicale configuration in TOML-compatible Nix form. This maps directly
        to the documented TOML configuration. Values set here override the
        module defaults.

        Durations must be strings such as `"15m"` and sizes strings such as
        `"10MiB"`.
      '';
      example = lib.literalExpression ''
        {
          server = {
            bind = [ "127.0.0.1:5232" ];
            external_url = "https://calendar.example.com";
          };
          auth = {
            backend = "local";
            password_file = "/run/secrets/oxicale/users";
          };
          calendar.default_timezone = "Europe/Berlin";
          subscriptions.enable = true;
        }
      '';
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Extra arguments passed to `oxicale run`.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Optional environment file for secrets such as `OXICALE_SECRET_KEY`.
        The file must be readable by the service user.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the main HTTP port in the NixOS firewall.";
    };

    firewallPort = lib.mkOption {
      type = lib.types.port;
      default = 5232;
      description = "Port to open when `openFirewall` is enabled.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    users.users = lib.mkIf (cfg.user == "oxicale") {
      oxicale = {
        isSystemUser = true;
        group = cfg.group;
      };
    };

    users.groups = lib.mkIf (cfg.group == "oxicale") {
      oxicale = { };
    };

    systemd.tmpfiles.rules = lib.optional (!usesDefaultStateDir) (
      "d ${dataDir} 0700 ${cfg.user} ${cfg.group} -"
    );

    systemd.services.oxicale = {
      description = "Oxicale CalDAV and CardDAV server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      requires = [ "network.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStartPre = "${cfg.package}/bin/oxicale check-config --config ${configFile}";
        ExecStart = lib.concatStringsSep " " (
          [ "${cfg.package}/bin/oxicale" "run" "--config" configFile ] ++ extraArgs
        );
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = dataDir;
        Restart = "on-failure";
        RestartSec = "5s";
        UMask = "0077";

        ReadWritePaths = [ dataDir ];
      }
      // lib.optionalAttrs usesDefaultStateDir {
        StateDirectory = "oxicale";
        StateDirectoryMode = "0700";
      }
      // lib.optionalAttrs (cfg.environmentFile != null) {
        EnvironmentFile = cfg.environmentFile;
      }
      // {
        # Hardening. Outbound network access is intentionally left enabled
        # because server-side subscriptions need it.
        CapabilityBoundingSet = [ "" ];
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        PrivateUsers = true;
        ProcSubset = "pid";
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        RemoveIPC = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
      };
    };

    networking.firewall.allowedTCPPorts = lib.optional cfg.openFirewall cfg.firewallPort;
  };
}
