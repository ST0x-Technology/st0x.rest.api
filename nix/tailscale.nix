# Enrolls the droplet onto the rain tailnet (taile5cf8a.ts.net) purely as a
# telemetry backhaul to the rain observability box: the obs droplet scrapes the
# app's Prometheus `/metrics` on `:8001`, and the app pushes OTLP logs/traces to
# `rain-management-observability:9428` (VictoriaLogs) / `:10428` (VictoriaTraces).
#
# Unlike the liquidity bot we do NOT serve anything over the tailnet — the public
# API keeps its nginx + Let's Encrypt vhost on `api.st0x.io`. So there is no
# tailscale HTTPS cert provisioning here; this file only joins the tailnet and
# opens the WireGuard path.
#
# The node joins with a per-environment, agenix-encrypted `tag:st0x-rest-api`
# auth key minted on the rain tailnet (used only on first enrollment; afterwards
# tailscaled re-auths via the stored node key in /var/lib/tailscale). The tag
# comes from the auth key, so no
# `--advertise-tags` is needed. `--hostname` pins the MagicDNS name to the devops
# scrape target. `tailscale0` is a trusted firewall interface, so the obs box can
# reach `:8001` without that port ever being opened to the public internet.
{
  pkgs,
  st0xEnv,
  ...
}:

let
  environment = st0xEnv.name;
  tailnetHostname = st0xEnv.tailnetHostname;
in
{
  services.tailscale = {
    enable = true;
    authKeyFile = "/run/agenix/tailscale-authkey-${environment}";
    extraUpFlags = [ "--hostname=${tailnetHostname}" ];
  };

  networking.firewall = {
    allowedUDPPorts = [
      41641 # Tailscale WireGuard
    ];
    trustedInterfaces = [ "tailscale0" ];
  };

  age.secrets."tailscale-authkey-${environment}" = {
    file = ../secret/tailscale-authkey-${environment}.age;
    mode = "0400";
  };

  # Clean up a stale tailscale0 TUN device before tailscaled starts: during NixOS
  # activation the previous tailscaled may still hold /dev/net/tun, which would
  # otherwise crash-loop the new unit.
  systemd.services.tailscaled.serviceConfig.ExecStartPre = [
    "-${pkgs.iproute2}/bin/ip link delete tailscale0"
  ];
}
