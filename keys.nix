rec {
  keys = {
    st0x-op =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPZ56nOYbGDd0ZfbqxeY7AbvaQGQrHnlC80ccpRGpCoj";
    host =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN1JMILASAjU2qxDdKpdwprx+GllpRWDneNk7dazY3uY";
    ci =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPTd2zKSwHgWegi290EiK5nYp1Wp4+x2fDYqFxbd0WLN";
    arda =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAyTREGZCOzMsl7N9dp1saN/t7DCs7YesusVUKApMJ78";
    sid = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPl3/6RlR6Rvz0ZRyZukzFtt4zUYNz5OVuTsajJl7V3n";
    alastair =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJArH3PA+bFIon0JkCVQGs9aWr45lnVjiiTLLO9BPItn";
  };

  roles = with keys; {
    infra = [ st0x-op ci sid alastair ];
    ssh = [ st0x-op ci arda sid alastair ];
  };
}
