output "ingest_endpoint" {
  description = "What the client should be pointed at"
  value = var.zone_id == "" ? (
    "https://${cloudflare_workers_script.ingest.script_name}.<your-subdomain>.workers.dev/v1/sessions"
    ) : (
    "https://ingest.${var.zone_name}/v1/sessions"
  )
}

output "bucket" {
  value = cloudflare_r2_bucket.cratebank.name
}
