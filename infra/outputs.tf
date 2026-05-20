output "droplet_id" {
  description = "ID of the NixOS droplet"
  value       = digitalocean_droplet.nixos.id
}

output "droplet_ipv4" {
  description = "Public IPv4 address of the droplet"
  value       = digitalocean_droplet.nixos.ipv4_address
}

output "reserved_ip" {
  description = "Reserved IP address assigned to the droplet"
  value       = digitalocean_reserved_ip.nixos.ip_address
}

output "volume_id" {
  description = "ID of the data volume"
  value       = digitalocean_volume.data.id
}

output "preview_droplet_id" {
  description = "ID of the preview NixOS droplet"
  value       = var.preview_enabled ? digitalocean_droplet.preview_nixos[0].id : null
}

output "preview_droplet_ipv4" {
  description = "Public IPv4 address of the preview droplet"
  value       = var.preview_enabled ? digitalocean_droplet.preview_nixos[0].ipv4_address : null
}

output "preview_reserved_ip" {
  description = "Reserved IP address assigned to the preview droplet"
  value       = var.preview_enabled ? digitalocean_reserved_ip.preview_nixos[0].ip_address : null
}

output "preview_volume_id" {
  description = "ID of the preview data volume"
  value       = var.preview_enabled ? digitalocean_volume.preview_data[0].id : null
}
