{ pkgs, lib, modulesPath, docsRoot, ... }:

let
  inherit (import ./keys.nix) roles;

  services = import ./services.nix;
  enabledServices = lib.filterAttrs (_: v: v.enabled) services;

  mkService = name: cfg:
    let
      path = "/nix/var/nix/profiles/per-service/${name}/bin/${cfg.bin}";
      configFile = ./config/${name}.toml;
    in {
      description = "st0x ${cfg.bin} (${name})";

      wantedBy = [ ];

      restartIfChanged = false;
      stopIfChanged = false;

      unitConfig = {
        "X-OnlyManualStart" = true;
        ConditionPathExists = "/run/st0x/${name}.ready";
      };

      serviceConfig = {
        DynamicUser = true;
        SupplementaryGroups = [ "st0x" ];
        ExecStart = "${path} serve --config ${configFile}";
        Restart = "always";
        RestartSec = 5;
        ReadWritePaths = [ "/mnt/data" ];
      };
    };

in {
  imports = [
    (modulesPath + "/virtualisation/digital-ocean-config.nix")
    (modulesPath + "/profiles/qemu-guest.nix")
    ./disko.nix
  ];

  boot.loader.grub = {
    efiSupport = true;
    efiInstallAsRemovable = true;
  };

  networking.useDHCP = lib.mkForce false;

  services = {
    cloud-init = {
      enable = true;
      network.enable = true;
      settings = {
        datasource_list = [ "ConfigDrive" "Digitalocean" ];
        datasource.ConfigDrive = { };
        datasource.Digitalocean = { };
        cloud_init_modules = [
          "seed_random"
          "bootcmd"
          "write_files"
          "growpart"
          "resizefs"
          "set_hostname"
          "update_hostname"
          "set_password"
        ];
        cloud_config_modules =
          [ "ssh-import-id" "keyboard" "runcmd" "disable_ec2_metadata" ];
        cloud_final_modules = [
          "write_files_deferred"
          "puppet"
          "chef"
          "ansible"
          "mcollective"
          "salt_minion"
          "reset_rmc"
          "scripts_per_once"
          "scripts_per_boot"
          "scripts_user"
          "ssh_authkey_fingerprints"
          "keys_to_console"
          "install_hotplug"
          "phone_home"
          "final_message"
        ];
      };
    };

    openssh = {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };

    nginx = {
      enable = true;
      recommendedTlsSettings = true;
      recommendedProxySettings = true;
      recommendedOptimisation = true;
      recommendedGzipSettings = true;

      # Rate-limit zone: 10 req/s per IP, burst 20
      appendHttpConfig = ''
        limit_req_zone $binary_remote_addr zone=api:10m rate=10r/s;
      '';

      virtualHosts."api.st0x.io" = {
        enableACME = true;
        forceSSL = true;

        extraConfig = ''
          # Security headers
          add_header X-Content-Type-Options "nosniff" always;
          add_header X-Frame-Options "DENY" always;
          add_header Referrer-Policy "strict-origin-when-cross-origin" always;

          # Limit request body size (API payloads are small)
          client_max_body_size 1m;
        '';

        # Block common exploit scanners (PHP, Docker, ThinkPHP, etc.)
        locations."~ \\.(php|asp|aspx|jsp|cgi)$" = {
          return = "444";
        };
        locations."~ ^/(containers|_ignition|vendor|public/index)" = {
          return = "444";
        };

        locations."/" = {
          proxyPass = "http://127.0.0.1:8000";
          extraConfig = ''
            limit_req zone=api burst=20 nodelay;
            limit_req_status 429;
          '';
        };
      };
    };
  };

  users.users.root.openssh.authorizedKeys.keys = roles.ssh;

  security.acme = {
    acceptTerms = true;
    defaults.email = "ops@st0x.io";
  };

  networking.firewall = {
    enable = true;
    allowedTCPPorts = [
      22
      80
      443
    ];
  };

  fileSystems."/mnt/data" = {
    device = "/dev/disk/by-id/scsi-0DO_Volume_st0x-rest-api-data-v2";
    fsType = "ext4";
  };

  nix = {
    settings = {
      experimental-features = [ "nix-command" "flakes" ];
      auto-optimise-store = true;
      download-buffer-size = 268435456;
      sandbox = false;
    };

    gc = {
      automatic = true;
      dates = "weekly";
      options = "--delete-older-than 30d";
    };
  };

  users.groups.st0x = { };
  programs.bash.interactiveShellInit = "set -o vi";

  services.logrotate = {
    enable = true;
    settings."/mnt/data/st0x-rest-api/logs/*.log" = {
      su = "root st0x";
      rotate = 14;
      weekly = true;
      compress = true;
      missingok = true;
      notifempty = true;
    };
  };

  systemd.tmpfiles.rules = [
    "d /mnt/data/st0x-rest-api 0775 root st0x -"
    "d /mnt/data/st0x-rest-api/logs 0775 root st0x -"
  ];
  systemd.services = lib.mapAttrs mkService enabledServices;

  environment.systemPackages = with pkgs; [
    bat
    curl
    htop
    sqlite
    zellij
  ];

  system.activationScripts.per-service-profiles.text =
    "mkdir -p /nix/var/nix/profiles/per-service";

  system.activationScripts.st0x-docs.text = ''
    ln -sfn ${docsRoot} /var/lib/st0x-docs
  '';

  system.stateVersion = "24.11";
}
