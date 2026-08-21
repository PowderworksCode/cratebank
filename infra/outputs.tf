output "ingest_endpoint" {
  description = "What the client should be pointed at"
  value       = var.zone_id == "" ? "https://${cloudflare_pipeline_stream.sessions.endpoint}" : "https://ingest.${var.zone_name}"
}

output "stream_endpoint" {
  description = "The Cloudflare hostname behind the CNAME"
  value       = cloudflare_pipeline_stream.sessions.endpoint
}

output "bucket" {
  description = "Raw payloads land here"
  value       = cloudflare_r2_bucket.cratebank.name
}

output "published_bucket" {
  description = "Compaction writes tables here"
  value       = cloudflare_r2_bucket.published.name
}
