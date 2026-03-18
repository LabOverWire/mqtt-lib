variable "project_id" {
  description = "GCP project ID"
  type        = string
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "us-east1"
}

variable "zone" {
  description = "GCP zone (must be within region)"
  type        = string
  default     = "us-east1-b"
}

variable "machine_type" {
  description = "GCE machine type for both instances"
  type        = string
  default     = "e2-standard-4"
}

variable "ssh_public_key_path" {
  description = "Path to SSH public key for instance access"
  type        = string
  default     = "~/.ssh/id_ed25519.pub"
}

variable "ssh_user" {
  description = "SSH username for instance access"
  type        = string
  default     = "bench"
}
