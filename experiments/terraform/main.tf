terraform {
  required_version = ">= 1.5"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
  zone    = var.zone
}

locals {
  ssh_key = "${var.ssh_user}:${file(pathexpand(var.ssh_public_key_path))}"
}

resource "google_compute_instance" "broker" {
  name         = "mqoq-broker"
  machine_type = var.machine_type

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 50
      type  = "pd-ssd"
    }
  }

  network_interface {
    network = "default"
    access_config {}
  }

  metadata = {
    ssh-keys = local.ssh_key
  }

  tags = ["mqoq-bench"]
}

resource "google_compute_instance" "client" {
  name         = "mqoq-client"
  machine_type = var.machine_type

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 50
      type  = "pd-ssd"
    }
  }

  network_interface {
    network = "default"
    access_config {}
  }

  metadata = {
    ssh-keys = local.ssh_key
  }

  tags = ["mqoq-bench"]
}

resource "google_compute_firewall" "mqoq_bench" {
  name    = "mqoq-bench-allow"
  network = "default"

  allow {
    protocol = "tcp"
    ports    = ["22", "1883", "8883", "14567"]
  }

  allow {
    protocol = "udp"
    ports    = ["14567"]
  }

  source_ranges = ["0.0.0.0/0"]
  target_tags   = ["mqoq-bench"]
}
