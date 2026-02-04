{
  description = "st0x REST API";

  inputs = {
    rainix.url = "github:rainlanguage/rainix";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, flake-utils, rainix }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = rainix.pkgs.${system};
      in rec {
        packages = rec {
          rs-test = rainix.mkTask.${system} {
            name = "rs-test";
            body = ''
              set -euxo pipefail
              cargo test --workspace
            '';
          };
        } // rainix.packages.${system};

        devShells.default = pkgs.mkShell {
          packages = [
            packages.rs-test
          ];

          shellHook = rainix.devShells.${system}.default.shellHook;
          buildInputs = rainix.devShells.${system}.default.buildInputs;
          nativeBuildInputs = rainix.devShells.${system}.default.nativeBuildInputs;
        };
      });
}
