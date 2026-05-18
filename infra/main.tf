data "digitalocean_ssh_key" "deploy" {
  name = var.ssh_key_name
}

resource "digitalocean_volume" "data" {
  region                  = var.region
  name                    = "st0x-rest-api-data"
  size                    = var.volume_size_gb
  initial_filesystem_type = "ext4"
  description             = "Persistent storage for SQLite database and logs"
}

resource "digitalocean_droplet" "nixos" {
  image    = "ubuntu-24-04-x64"
  name     = "st0x-rest-api-nixos"
  region   = var.region
  size     = var.droplet_size
  ssh_keys = [data.digitalocean_ssh_key.deploy.id]
}

resource "digitalocean_volume_attachment" "data" {
  droplet_id = digitalocean_droplet.nixos.id
  volume_id  = digitalocean_volume.data.id
}

resource "digitalocean_reserved_ip" "nixos" {
  region = var.region
}

resource "digitalocean_reserved_ip_assignment" "nixos" {
  ip_address = digitalocean_reserved_ip.nixos.ip_address
  droplet_id = digitalocean_droplet.nixos.id
}

resource "digitalocean_volume" "preview_data" {
  count = var.preview_enabled ? 1 : 0

  region                  = var.region
  name                    = "st0x-rest-api-preview-data"
  size                    = var.preview_volume_size_gb
  initial_filesystem_type = "ext4"
  description             = "Persistent preview storage for SQLite database and logs"
}

resource "digitalocean_droplet" "preview_nixos" {
  count = var.preview_enabled ? 1 : 0

  image    = "ubuntu-24-04-x64"
  name     = "st0x-rest-api-preview-nixos"
  region   = var.region
  size     = var.preview_droplet_size
  ssh_keys = [data.digitalocean_ssh_key.deploy.id]
}

resource "digitalocean_volume_attachment" "preview_data" {
  count = var.preview_enabled ? 1 : 0

  droplet_id = digitalocean_droplet.preview_nixos[0].id
  volume_id  = digitalocean_volume.preview_data[0].id
}

resource "digitalocean_reserved_ip" "preview_nixos" {
  count = var.preview_enabled ? 1 : 0

  region = var.region
}

resource "digitalocean_reserved_ip_assignment" "preview_nixos" {
  count = var.preview_enabled ? 1 : 0

  ip_address = digitalocean_reserved_ip.preview_nixos[0].ip_address
  droplet_id = digitalocean_droplet.preview_nixos[0].id
}
