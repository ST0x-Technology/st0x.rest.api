{ deploy-rs, self }:

let
  system = "x86_64-linux";
  inherit (deploy-rs.lib.${system}) activate;
  profileBase = "/nix/var/nix/profiles/per-service";

  st0xPackage = self.packages.${system}.st0x-rest-api;

  services = import ./services.nix;
  enabledServices = builtins.attrNames (builtins.removeAttrs services
    (builtins.filter (n: !services.${n}.enabled)
      (builtins.attrNames services)));

  mkServiceProfile = { name, resetState ? false, dataDir ? null }:
    let
      markerFile = "/run/st0x/${name}.ready";
      resetCommands = if resetState then [
        "rm -f ${dataDir}/st0x.db ${dataDir}/raindex.db"
      ] else [ ];
    in activate.custom st0xPackage (builtins.concatStringsSep " && " ([
      "systemctl stop ${name} || true"
      "rm -f ${markerFile}"
    ] ++ resetCommands ++ [
      "mkdir -p /run/st0x"
      "touch ${markerFile}"
      "systemctl restart ${name}"
    ]));

  mkProfile = { name, resetState ? false, dataDir ? null }: {
    path = mkServiceProfile { inherit name resetState dataDir; };
    profilePath = "${profileBase}/${name}";
  };

  mkProfiles = { resetState ? false, dataDir ? null }:
    builtins.listToAttrs (map (name: {
      inherit name;
      value = mkProfile { inherit name resetState dataDir; };
    }) enabledServices);

in {
  config = {
    nodes.st0x-rest-api = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path = activate.nixos self.nixosConfigurations.st0x-rest-api-prod;
      } // mkProfiles { };
    };

    nodes.st0x-rest-api-preview = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path = activate.nixos self.nixosConfigurations.st0x-rest-api-preview;
      } // mkProfiles { };
    };
  };

  wrappers = { pkgs, infraPkgs, localSystem }:
    let
      deployInputs = infraPkgs.buildInputs
        ++ [ deploy-rs.packages.${localSystem}.deploy-rs ];

      deployPreamble = ''
        ${infraPkgs.resolveIp}
        export DEPLOY_HOST="$host_ip"
        export NIX_SSHOPTS="-i $identity"
        ssh_flag="--ssh-opts=-i $identity"
      '';

      previewDeployPreamble = ''
        export DEPLOY_ENV=preview
        ${deployPreamble}
      '';

      deployFlags = if localSystem == "x86_64-linux" then
        ""
      else
        "--skip-checks --remote-build";

    in {
      deployNixos = pkgs.writeShellApplication {
        name = "deploy-nixos";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#st0x-rest-api.system \
            -- --impure "$@"
        '';
      };

      deployService = pkgs.writeShellApplication {
        name = "deploy-service";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          profile="''${1:?usage: deploy-service <profile>}"
          shift
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} ".#st0x-rest-api.$profile" \
            -- --impure "$@"
        '';
      };

      deployAll = pkgs.writeShellApplication {
        name = "deploy-all";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#st0x-rest-api \
            -- --impure "$@"
        '';
      };

      deployPreviewNixos = pkgs.writeShellApplication {
        name = "deploy-preview-nixos";
        runtimeInputs = deployInputs;
        text = ''
          ${previewDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#st0x-rest-api-preview.system \
            -- --impure "$@"
        '';
      };

      deployPreviewService = pkgs.writeShellApplication {
        name = "deploy-preview-service";
        runtimeInputs = deployInputs;
        text = ''
          ${previewDeployPreamble}
          profile="''${1:-rest-api}"
          shift || true
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} ".#st0x-rest-api-preview.$profile" \
            -- --impure "$@"
        '';
      };

      deployPreviewAll = pkgs.writeShellApplication {
        name = "deploy-preview-all";
        runtimeInputs = deployInputs;
        text = ''
          ${previewDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#st0x-rest-api-preview \
            -- --impure "$@"
        '';
      };
    };
}
