{
  pkgs,
  ragenix,
  rainix,
  system,
}:

let
  buildInputs = [
    pkgs.terraform
    pkgs.rage
    pkgs.jq
    ragenix.packages.${system}.default
  ];

  tfState = "infra/terraform.tfstate";
  tfVars = "infra/terraform.tfvars";
  tfSecretVars = "infra/zz_secret.auto.tfvars";
  tfPlanFile = "infra/tfplan";
  previewVolumeAddr = "digitalocean_volume.preview_data[0]";
  previewVolumeId = "475f705e-54f8-11f1-9077-0a58ac129076";
  previewDropletAddr = "digitalocean_droplet.preview_nixos[0]";
  previewDropletId = "572288021";
  previewReservedIpAddr = "digitalocean_reserved_ip.preview_nixos[0]";
  previewReservedIp = "138.197.53.151";

  parseIdentity = ''
    set -eo pipefail

    identity=~/.ssh/id_ed25519
    if [ "''${1:-}" = "-i" ]; then
      identity="$2"
      shift 2
    fi
  '';

  decryptState = ''
    if [ -f ${tfState}.age ]; then
      rage -d -i "$identity" ${tfState}.age > ${tfState}
    fi
  '';

  encryptState = ''
    if [ -f ${tfState} ]; then
      nix eval --raw --file ${../keys.nix} roles.ssh --apply 'builtins.concatStringsSep "\n"' \
        | rage -e -R /dev/stdin -o ${tfState}.age ${tfState}
    fi
  '';

  cleanup = "rm -f ${tfState} ${tfState}.backup ${tfVars} ${tfSecretVars}";
  cleanupWithPlan = "${cleanup} ${tfPlanFile}";

  preamble = ''
    ${parseIdentity}
    on_exit() { ${cleanup}; }
    trap on_exit EXIT
    ${decryptVars}
  '';

  preambleWithEncrypt = ''
    ${parseIdentity}
    on_exit() {
      ${encryptState}
      ${cleanupWithPlan}
    }
    trap on_exit EXIT
    ${decryptVars}
  '';

  resolveIp = ''
    ${parseIdentity}
    trap 'rm -f ${tfState}' EXIT
    ${decryptState}
    deploy_env="''${DEPLOY_ENV:-prod}"
    case "$deploy_env" in
      prod)
        host_ip=$(jq -r '.outputs.reserved_ip.value // empty' ${tfState})
        if [ -z "$host_ip" ] || [ "$host_ip" = "null" ]; then
          echo "prod infrastructure is not present in Terraform state" >&2
          exit 1
        fi
        ;;
      preview)
        host_ip=$(jq -r '.outputs.preview_reserved_ip.value // empty' ${tfState})
        if [ -z "$host_ip" ] || [ "$host_ip" = "null" ]; then
          echo "preview infrastructure is not present in Terraform state" >&2
          exit 1
        fi
        ;;
      *)
        echo "unsupported DEPLOY_ENV '$deploy_env' (expected prod or preview)" >&2
        exit 1
        ;;
    esac
    rm -f ${tfState}
  '';

  decryptVars = ''
    rage -d -i "$identity" ${tfVars}.age > ${tfVars}
    if [ -n "''${TF_VAR_do_token:-}" ]; then
      printf 'do_token = "%s"\n' "$TF_VAR_do_token" > ${tfSecretVars}
    fi
  '';

  encryptVars = ''
    nix eval --raw --file ${../keys.nix} roles.infra --apply 'builtins.concatStringsSep "\n"' \
      | rage -e -R /dev/stdin -o ${tfVars}.age ${tfVars}
  '';

  tfRekey = ''
    ${parseIdentity}
    on_exit() { ${cleanup}; }
    trap on_exit EXIT
    ${decryptState}
    ${encryptState}
    ${decryptVars}
    ${encryptVars}
  '';

  tfPreviewProvision = ''
    ${preambleWithEncrypt}
    ${decryptState}

    import_if_needed() {
      local addr="$1"
      local id="$2"
      local allow_missing="''${3:-false}"
      local output

      if output=$(terraform -chdir=infra import \
        -var preview_enabled=true "$addr" "$id" 2>&1); then
        echo "$output"
        return 0
      fi

      if echo "$output" | grep -qiE \
        'already managed by Terraform|Resource already managed|already exists in the state'; then
        echo "Import skipped for $addr: already managed"
        return 0
      fi

      if [ "$allow_missing" = "true" ] && echo "$output" | grep -qiE \
        '404|could not be found|Cannot import non-existent remote object'; then
        echo "Import skipped for $addr: remote resource $id is missing; Terraform will create it"
        return 0
      fi

      echo "$output" >&2
      return 1
    }

    terraform -chdir=infra init
    import_if_needed '${previewVolumeAddr}' '${previewVolumeId}'
    import_if_needed '${previewDropletAddr}' '${previewDropletId}' true
    import_if_needed '${previewReservedIpAddr}' '${previewReservedIp}'

    if [ "''${RECREATE_PREVIEW_HOST:-false}" = "true" ]; then
      terraform -chdir=infra plan \
        -out=tfplan \
        -var preview_enabled=true \
        -replace='${previewDropletAddr}'
    else
      terraform -chdir=infra plan -out=tfplan -var preview_enabled=true
    fi

    terraform -chdir=infra apply tfplan
  '';

in
{
  inherit
    buildInputs
    parseIdentity
    resolveIp
    tfRekey
    tfPreviewProvision
    ;

  tfInit = rainix.mkTask.${system} {
    name = "tf-init";
    additionalBuildInputs = buildInputs;
    body = ''
      ${preamble}
      terraform -chdir=infra init "$@"
    '';
  };

  tfPlan = rainix.mkTask.${system} {
    name = "tf-plan";
    additionalBuildInputs = buildInputs;
    body = ''
      ${preamble}
      ${decryptState}
      terraform -chdir=infra plan -out=tfplan "$@"
    '';
  };

  tfApply = rainix.mkTask.${system} {
    name = "tf-apply";
    additionalBuildInputs = buildInputs;
    body = ''
      ${preambleWithEncrypt}
      ${decryptState}
      terraform -chdir=infra apply "$@" tfplan
    '';
  };

  tfImport = rainix.mkTask.${system} {
    name = "tf-import";
    additionalBuildInputs = buildInputs;
    body = ''
      ${preambleWithEncrypt}
      ${decryptState}
      terraform -chdir=infra import "$@"
    '';
  };

  tfDestroy = rainix.mkTask.${system} {
    name = "tf-destroy";
    additionalBuildInputs = buildInputs;
    body = ''
      ${preambleWithEncrypt}
      ${decryptState}
      terraform -chdir=infra destroy "$@"
    '';
  };

  tfEditVars = rainix.mkTask.${system} {
    name = "tf-edit-vars";
    additionalBuildInputs = buildInputs;
    body = ''
      ${parseIdentity}
      on_exit() { rm -f ${tfVars}; }
      trap on_exit EXIT

      if [ -f ${tfVars}.age ]; then
        ${decryptVars}
      else
        cp ${tfVars}.example ${tfVars}
      fi
      ''${EDITOR:-vi} ${tfVars}
      ${encryptVars}
    '';
  };
}
