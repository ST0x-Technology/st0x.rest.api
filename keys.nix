rec {
  keys = {
    st0x-op =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPZ56nOYbGDd0ZfbqxeY7AbvaQGQrHnlC80ccpRGpCoj";
    host =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK9JhlVsHGlSS3c+RGKFSwXyuFpvUTbnOny9e2AdBQ6G";
    ci =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPTd2zKSwHgWegi290EiK5nYp1Wp4+x2fDYqFxbd0WLN";
    arda =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAyTREGZCOzMsl7N9dp1saN/t7DCs7YesusVUKApMJ78";
    sid = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPl3/6RlR6Rvz0ZRyZukzFtt4zUYNz5OVuTsajJl7V3n";
    alastair =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJArH3PA+bFIon0JkCVQGs9aWr45lnVjiiTLLO9BPItn";

    # Preview droplet SSH host public key. `host` above is the prod droplet's.
    # The preview box is reprovisioned dynamically and its host key is supplied
    # to deploys via the PREVIEW_SSH_HOST_KEY secret (see deploy-preview.yaml).
    # TODO(dev handoff): replace with the preview droplet's real host key so
    # `secret/tailscale-authkey-preview.age` can be encrypted to it — until then
    # the preview auth-key secret cannot be created.
    host-preview = "ssh-ed25519 REPLACE_WITH_PREVIEW_DROPLET_HOST_KEY";
  };

  roles = with keys; {
    infra = [ st0x-op ci arda sid alastair ];
    ssh = [ st0x-op ci arda sid alastair ];

    # Recipients for runtime agenix secrets, per environment: an admin key (to
    # edit the secret) plus that droplet's own host key (to decrypt it at NixOS
    # activation). Used by secret/secrets.nix for the tailscale auth keys.
    service = {
      prod = [ st0x-op host ];
      preview = [ st0x-op host-preview ];
    };
  };
}
