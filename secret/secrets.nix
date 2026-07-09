# agenix/ragenix rules for runtime secrets on the deployed droplets.
#
# Currently just the per-environment Tailscale auth keys used to enrol each box
# onto the st0x tailnet (nix/tailscale.nix reads them at
# /run/agenix/tailscale-authkey-${environment}). Each is a reusable, tagged
# `tag:st0x-rest-api` key, encrypted to that environment's service recipients:
# an admin key (to edit) + the droplet's host key (to decrypt at activation).
#
# DEV HANDOFF — creating the .age files is done by the devs, not committed here:
#   1. Mint a reusable, tagged `tag:st0x-rest-api` auth key in the Tailscale
#      admin console (one per environment). Requires the tag:st0x-rest-api
#      tagOwner to exist in st0x.devops terraform/tailscale/policy.hujson.
#   2. From this directory:
#        printf %s '<tskey-...>' | ragenix -e tailscale-authkey-prod.age
#        printf %s '<tskey-...>' | ragenix -e tailscale-authkey-preview.age
#      (preview also needs keys.nix `host-preview` filled with the real preview
#      droplet host key first — see keys.nix.)
let
  inherit (import ../keys.nix) roles;
in
{
  "tailscale-authkey-prod.age".publicKeys = roles.service.prod;
  "tailscale-authkey-preview.age".publicKeys = roles.service.preview;
}
