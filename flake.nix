{
  description = "st0x REST API";

  inputs = {
    rainix.url = "github:rainlanguage/rainix";
    flake-utils.url = "github:numtide/flake-utils";
    ragenix.url = "github:yaxitech/ragenix";
    deploy-rs.url = "github:serokell/deploy-rs";

    crane.url = "github:ipetkov/crane";

    disko.url = "github:nix-community/disko";
    disko.inputs.nixpkgs.follows = "rainix/nixpkgs";

    nixos-anywhere.url = "github:nix-community/nixos-anywhere";
    nixos-anywhere.inputs.nixpkgs.follows = "rainix/nixpkgs";
  };

  outputs = { self, flake-utils, rainix, ragenix, deploy-rs, disko
    , nixos-anywhere, crane, ... }:
    {
      nixosConfigurations.st0x-rest-api =
        rainix.inputs.nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";

          modules =
            [ disko.nixosModules.disko ragenix.nixosModules.default ./os.nix ];
        };

      deploy = (import ./deploy.nix { inherit deploy-rs self; }).config;

      checks.x86_64-linux = deploy-rs.lib.x86_64-linux.deployChecks self.deploy;
    } // flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import rainix.inputs.nixpkgs {
          inherit system;
          config.allowUnfreePredicate = pkg:
            builtins.elem (pkgs.lib.getName pkg) [ "terraform" ];
        };

        craneLib =
          (crane.mkLib pkgs).overrideToolchain rainix.rust-toolchain.${system};
      in rec {
        packages = let
          rainixPkgs = rainix.packages.${system};
          infraPkgs = import ./infra { inherit pkgs ragenix rainix system; };

          deployPkgs =
            (import ./deploy.nix { inherit deploy-rs self; }).wrappers {
              inherit pkgs infraPkgs;
              localSystem = system;
            };

          st0xRust = pkgs.callPackage ./rust.nix {
            inherit craneLib;
            inherit (pkgs) sqlx-cli;
          };
        in rainixPkgs // deployPkgs // {
          inherit (infraPkgs) tfInit tfPlan tfApply tfDestroy tfEditVars;

          st0x-rest-api = st0xRust.package;
          st0x-clippy = st0xRust.clippy;

          prepSolArtifacts = rainix.mkTask.${system} {
            name = "prep-sol-artifacts";
            additionalBuildInputs = rainix.sol-build-inputs.${system};
            body = ''
              set -euxo pipefail

              (cd lib/rain.orderbook/ && forge build)
              (cd lib/rain.orderbook/lib/rain.orderbook.interface/lib/rain.interpreter.interface/lib/rain.math.float/ && forge build)
            '';
          };

          bootstrap = rainix.mkTask.${system} {
            name = "bootstrap-nixos";
            additionalBuildInputs = infraPkgs.buildInputs
              ++ [ nixos-anywhere.packages.${system}.default ];
            body = ''
              ${infraPkgs.resolveIp}
              ssh_opts="-o StrictHostKeyChecking=no -o ConnectTimeout=5 -i $identity"

              nixos-anywhere --flake ".#st0x-rest-api" \
                --option pure-eval false \
                --ssh-option "IdentityFile=$identity" \
                --target-host "root@$host_ip" "$@"

              echo "Waiting for host to come back up..."
              retries=0
              until ssh $ssh_opts "root@$host_ip" true 2>/dev/null; do
                retries=$((retries + 1))
                if [ "$retries" -ge 60 ]; then
                  echo "Host did not come back up after 5 minutes" >&2
                  exit 1
                fi
                sleep 5
              done

              new_key=$(
                ssh $ssh_opts "root@$host_ip" \
                  cat /etc/ssh/ssh_host_ed25519_key.pub \
                  | awk '{print $1 " " $2}'
              )

              valid_key='^ssh-ed25519 [A-Za-z0-9+/=]+$'
              if [ -z "$new_key" ] || ! echo "$new_key" | grep -qE "$valid_key"; then
                echo "ERROR: SSH host key is empty or malformed: '$new_key'" >&2
                exit 1
              fi

              ${pkgs.gnused}/bin/sed -i \
                '/host =/{n;s|"ssh-ed25519 [A-Za-z0-9+/=]*"|"'"$new_key"'"|;}' \
                keys.nix

              echo "Updated host key in keys.nix"
            '';
          };

          tfRekey = rainix.mkTask.${system} {
            name = "tf-rekey";
            additionalBuildInputs = infraPkgs.buildInputs;
            body = infraPkgs.tfRekey;
          };

          resolveIp = pkgs.writeShellApplication {
            name = "resolve-ip";
            runtimeInputs = infraPkgs.buildInputs;
            text = ''
              ${infraPkgs.resolveIp}
              echo "$host_ip"
            '';
          };

          remote = pkgs.writeShellApplication {
            name = "remote";
            runtimeInputs = infraPkgs.buildInputs ++ [ pkgs.openssh ];
            text = ''
              ${infraPkgs.resolveIp}
              exec ssh -i "$identity" "root@$host_ip" "$@"
            '';
          };

        };

        formatter = pkgs.nixfmt-classic;

        devShells.default = pkgs.mkShell {
          inherit (rainix.devShells.${system}.default) nativeBuildInputs;
          inherit (rainix.devShells.${system}.default) shellHook;
          buildInputs = with pkgs;
            [
              sqlx-cli
              terraform
              ragenix.packages.${system}.default
              packages.prepSolArtifacts
              packages.remote
              packages.deployNixos
              packages.deployService
              packages.deployAll
            ] ++ rainix.devShells.${system}.default.buildInputs;
        };
      });

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };
}
