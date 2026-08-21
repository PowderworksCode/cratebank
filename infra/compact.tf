# Compaction: raw session blobs -> public parquet, and the public query surface.
#
# The raw uploads stay the ground truth; this produces the *convenient* form.
# Nightly, it reads every session and writes two flat parquet tables to a public
# path, so the census can be queried with one line and no credentials:
#
#   SELECT * FROM 'https://data.cratebank.io/units.parquet';

# Shared secret for the compactor's manual trigger. The nightly cron needs no
# credential; this only guards POST /compact, so a rebuild cannot be forced by
# anyone who finds the hostname.
variable "compact_secret" {
  description = "Bearer token for manually triggering compaction"
  type        = string
  sensitive   = true
}

# Deliberately a full rebuild rather than an incremental merge. At this scale it
# is free, and a reading that can be re-run over everything already collected is
# the property the whole ingest design is organised around -- a flattening bug
# is fixed by fixing the code and waiting a day, not by re-collecting data.
#
# worker/dist/compact.js is built from worker/compact.js by `npm run build`. It
# is committed so a fresh clone can apply without a node toolchain, and CI
# rebuilds it to check it still matches its source.
resource "cloudflare_workers_script" "compact" {
  account_id  = var.account_id
  script_name = "cratebank-compact"

  content     = file("${path.module}/worker/dist/compact.js")
  main_module = "compact.js"

  compatibility_date = "2026-08-21"

  bindings = [
    {
      name        = "BUCKET"
      type        = "r2_bucket"
      bucket_name = cloudflare_r2_bucket.cratebank.name
    },
    {
      name = "COMPACT_SECRET"
      type = "secret_text"
      text = var.compact_secret
    },
  ]

  # Declared in full: these sub-fields are optional+computed, and half-specifying
  # them makes Terraform re-upload the script on every plan. See README.
  observability = {
    enabled            = true
    head_sampling_rate = 1
    logs = {
      enabled            = true
      head_sampling_rate = 1
      invocation_logs    = true
      persist            = true
    }
    traces = {
      enabled            = false
      head_sampling_rate = 1
      persist            = true
    }
  }
}

# The compactor's only route is its workers.dev subdomain, used for the manual
# POST /compact trigger. It is not on cratebank.io on purpose: this is an admin
# endpoint, not part of the public surface, and it is guarded by a shared
# secret. Debugging a cron-only Worker without a way to invoke it is miserable.
resource "cloudflare_workers_script_subdomain" "compact" {
  account_id       = var.account_id
  script_name      = cloudflare_workers_script.compact.script_name
  enabled          = true
  previews_enabled = false
}

# 05:00 UTC. Cron triggers at >= 1 hour intervals get 15 minutes of CPU on
# Workers Paid, against 30 seconds for a fetch handler -- which is what makes a
# whole-dataset rebuild comfortable rather than tight.
resource "cloudflare_workers_cron_trigger" "compact" {
  account_id  = var.account_id
  script_name = cloudflare_workers_script.compact.script_name
  schedules   = [{ cron = "0 5 * * *" }]
}

# Serves the bucket at data.cratebank.io with no credential.
#
# This makes *every* object in the bucket world-readable, raw session blobs
# included -- which is intended (the census is public), but is not reversible
# for anything already fetched or indexed.
resource "cloudflare_r2_custom_domain" "data" {
  count = var.zone_id == "" ? 0 : 1

  account_id  = var.account_id
  bucket_name = cloudflare_r2_bucket.cratebank.name
  domain      = "data.${var.zone_name}"
  zone_id     = var.zone_id
  enabled     = true
  min_tls     = "1.2"
}
