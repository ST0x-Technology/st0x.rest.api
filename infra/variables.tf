variable "do_token" {
  description = "DigitalOcean API token"
  type        = string
  sensitive   = true
}

variable "ssh_key_name" {
  description = "Name of the SSH key in DigitalOcean to add to the droplet"
  type        = string
  default     = "st0x-op"
}

variable "region" {
  description = "DigitalOcean region"
  type        = string
  default     = "nyc3"
}

variable "droplet_size" {
  description = "Droplet size slug"
  type        = string
  default     = "s-2vcpu-4gb"
}

variable "volume_size_gb" {
  description = "Block storage volume size in GB"
  type        = number
  default     = 5
}

variable "preview_enabled" {
  description = "Whether to provision the reusable preview droplet, volume, and reserved IP"
  type        = bool
  default     = false
}

variable "preview_droplet_size" {
  description = "Droplet size slug for the preview environment"
  type        = string
  default     = "s-2vcpu-4gb"
}

variable "preview_volume_size_gb" {
  description = "Block storage volume size in GB for the preview environment"
  type        = number
  default     = 5
}

variable "preview_bootstrap_ssh_public_key" {
  description = "Public SSH key authorized on the preview droplet before NixOS bootstrap"
  type        = string
  default     = ""

  validation {
    condition = (
      var.preview_bootstrap_ssh_public_key == ""
      || can(regex("^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(256|384|521))\\s+[A-Za-z0-9+/=]+(\\s+.+)?$", var.preview_bootstrap_ssh_public_key))
    )
    error_message = "preview_bootstrap_ssh_public_key must be empty or a valid SSH public key."
  }
}
