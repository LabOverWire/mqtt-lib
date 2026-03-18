output "broker_ip" {
  description = "External IP of the broker instance"
  value       = google_compute_instance.broker.network_interface[0].access_config[0].nat_ip
}

output "client_ip" {
  description = "External IP of the client instance"
  value       = google_compute_instance.client.network_interface[0].access_config[0].nat_ip
}
