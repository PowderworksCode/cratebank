output "ingest_endpoint" {
  description = "What the client should be pointed at"
  value       = "https://ingest.${var.zone_name}"
}

output "stream_endpoint" {
  description = "The Cloudflare hostname behind the CNAME"
  value       = cloudflare_pipeline_stream.sessions.endpoint
}

output "bucket" {
  value = cloudflare_r2_bucket.cratebank.name
}
