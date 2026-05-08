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

  mkServiceProfile = name:
    let
      markerFile = "/run/st0x/${name}.ready";
    in activate.custom st0xPackage (builtins.concatStringsSep " && " [
      "systemctl stop ${name} || true"
      "rm -f ${markerFile}"
      "mkdir -p /run/st0x"
      "touch ${markerFile}"
      "systemctl restart ${name}"
    ]);

  mkProfile = name: {
    path = mkServiceProfile name;
    profilePath = "${profileBase}/${name}";
  };

in {
  config = {
    nodes.st0x-rest-api = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path = activate.nixos self.nixosConfigurations.st0x-rest-api;
      } // builtins.listToAttrs (map (name: {
        inherit name;
        value = mkProfile name;
      }) enabledServices);
    };
  };

  wrappers = { pkgs, infraPkgs, localSystem }:
    let
      deployInputs = infraPkgs.buildInputs
        ++ [ deploy-rs.packages.${localSystem}.deploy-rs ];
      sshInputs = infraPkgs.buildInputs ++ [ pkgs.openssh pkgs.coreutils ];

      hostPreamble = ''
        ${infraPkgs.resolveIp}
        export DEPLOY_HOST="$host_ip"
        export NIX_SSHOPTS="-i $identity"
      '';

      deployPreamble = ''
        ${hostPreamble}
        ssh_flag="--ssh-opts=-i $identity"
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

      deployRpcSecrets = pkgs.writeShellApplication {
        name = "deploy-rpc-secrets";
        runtimeInputs = sshInputs;
        text = ''
          ${hostPreamble}
          remote="root@$DEPLOY_HOST"
          remote_path="/mnt/data/st0x-rest-api/private-rpcs.txt"

          if [ -z "''${PRIVATE_RPC_URLS:-}" ]; then
            echo "PRIVATE_RPC_URLS is not set; deployment will use the public registry RPCs"
            ssh -i "$identity" "$remote" "rm -f $remote_path"
            exit 0
          fi

          tmp_file="$(mktemp)"
          trap 'rm -f "$tmp_file"' EXIT
          printf '%s' "$PRIVATE_RPC_URLS" > "$tmp_file"

          ssh -i "$identity" "$remote" \
            'install -d -m 0775 -o root -g st0x /mnt/data/st0x-rest-api'
          ssh -i "$identity" "$remote" \
            "umask 0177 && cat > $remote_path.tmp" < "$tmp_file"
          ssh -i "$identity" "$remote" \
            "chown root:st0x $remote_path.tmp && chmod 0640 $remote_path.tmp && mv $remote_path.tmp $remote_path"
        '';
      };
    };
}
