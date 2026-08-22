# cratebank ingest, as infrastructure.
#
# One R2 data bucket, two request-time Workers, static landing-page assets,
# their routes, and a daily cron. The ingest Worker puts request bodies in R2
# without decoding them; the compactor rebuilds the public parquet tables.

terraform {
  required_version = ">= 1.6"

  # The bucket and R2 endpoint are supplied by GitHub Actions so credentials
  # and account-specific values never land in this file. State belongs in a
  # private bucket, never the public data bucket: it contains the compactor's
  # secret binding.
  backend "s3" {
    key                         = "cratebank/terraform.tfstate"
    region                      = "auto"
    skip_credentials_validation = true
    skip_metadata_api_check     = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    skip_s3_checksum            = true
    use_path_style              = true
  }

  required_providers {
    cloudflare = {
      source = "cloudflare/cloudflare"
      # Pin the minor: this provider has several optional+computed attributes
      # whose server-side defaults Terraform reads as drift, and an upgrade
      # that adds another one turns into a surprise plan. See README.
      version = "~> 5.23"
    }
  }
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}

# ── storage ──────────────────────────────────────────────────────────────────

resource "cloudflare_r2_bucket" "cratebank" {
  account_id = var.account_id
  name       = var.bucket_name
  location   = var.bucket_location
}

# ── ingest Worker ────────────────────────────────────────────────────────────

# The client sends one object per session. The request ceiling is the zone's
# request-body limit (100 MB on Free/Pro).
#
# The script never decompresses or parses the body; it streams the bytes to R2
# unchanged. See worker/ingest.js.
resource "cloudflare_workers_script" "ingest" {
  account_id  = var.account_id
  script_name = "cratebank-ingest"

  content     = file("${path.module}/worker/ingest.js")
  main_module = "ingest.js"

  # Pinned: a compatibility date is the Workers runtime's schema version, and
  # letting it float means a runtime change can alter behaviour under us.
  compatibility_date = "2026-08-21"

  bindings = [{
    name        = "BUCKET"
    type        = "r2_bucket"
    bucket_name = cloudflare_r2_bucket.cratebank.name
  }]

  # Declared in full, not just `enabled = true`. These sub-fields are
  # optional+computed: the server fills in whatever is omitted, and Terraform
  # then reads the difference as a removal and re-uploads the script on every
  # plan.
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

# The hostname the client ships with. A Workers custom domain creates and
# manages its own DNS record, so this replaces the CNAME entirely -- do not
# also declare a cloudflare_dns_record for the same name.
#
# Needs *zone* permissions on the API token (Workers Routes · Edit and DNS ·
# Edit, scoped to the zone); the account-scoped token that builds everything
# else cannot see the zone at all.
resource "cloudflare_workers_custom_domain" "ingest" {
  count = var.zone_id == "" ? 0 : 1

  account_id = var.account_id
  zone_id    = var.zone_id
  hostname   = "ingest.${var.zone_name}"
  service    = cloudflare_workers_script.ingest.script_name
}
