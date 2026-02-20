rec {
  keys = {
    st0x-op =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPZ56nOYbGDd0ZfbqxeY7AbvaQGQrHnlC80ccpRGpCoj";
    host =
      "ssh-ed25519 PLACEHOLDER";
    ci =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIARWUchVuQvkFm2tzspdC79hhRyYbWzRjs5iimhxewUy";
  };

  roles = {
    infra = [ keys.st0x-op keys.ci ];
    ssh = [ keys.st0x-op keys.ci ];
  };
}
