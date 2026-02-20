rec {
  keys = {
    st0x-op =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPZ56nOYbGDd0ZfbqxeY7AbvaQGQrHnlC80ccpRGpCoj";
    host =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK9JhlVsHGlSS3c+RGKFSwXyuFpvUTbnOny9e2AdBQ6G";
    ci =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIARWUchVuQvkFm2tzspdC79hhRyYbWzRjs5iimhxewUy";
    arda =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAyTREGZCOzMsl7N9dp1saN/t7DCs7YesusVUKApMJ78";
  };

  roles = {
    infra = [ keys.st0x-op keys.ci ];
    ssh = [ keys.st0x-op keys.ci keys.arda ];
  };
}
