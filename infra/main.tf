# cratebank ingest, as infrastructure.
#
# One R2 bucket, one unstructured stream, one sink, one pipeline that connects
# them, and a CNAME so the client never learns Cloudflare's hostname.
#
# Everything the ingest design calls for is here; there is no server component
# and no application code.

terraform {
  required_version = ">= 1.6"
  required_providers {
    cloudflare = {
      source = "cloudflare/cloudflare"
      # Pipelines resources landed in 5.19; pin the minor so a provider
      # upgrade cannot silently change a schema underneath the stream.
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

# ── ingest ───────────────────────────────────────────────────────────────────

# Unstructured: a single `value` column holding whatever JSON arrives.
#
# This is deliberate rather than lazy. Structured streams cannot have their
# schema modified after creation, and cargo's log format is still moving — a
# declared schema would freeze today's field names into infrastructure we
# cannot alter, and silently drop every field cargo adds. Nothing is
# interpreted at ingest, so nothing can be lost by interpreting it wrongly.
resource "cloudflare_pipeline_stream" "sessions" {
  account_id = var.account_id
  name       = "cratebank_sessions"

  format = {
    type = "json"
  }

  schema = {
    fields = [{
      name     = "value"
      type     = "json"
      required = true
    }]
  }

  http = {
    enabled = true
    # v1 is authless: see docs/ingest.md. The endpoint stays unadvertised
    # until this flips, and every row is tagged trust: anonymous meanwhile,
    # so adding auth later is a filter rather than a migration.
    authentication = false
    cors           = {}
  }

  worker_binding = {
    enabled = false
  }
}

# ── sink ─────────────────────────────────────────────────────────────────────

resource "cloudflare_pipeline_sink" "raw" {
  account_id = var.account_id
  name       = "cratebank_raw"
  type       = "r2"

  format = {
    type            = "parquet"
    compression     = "zstd"
    row_group_bytes = 134217728 # 128 MiB
  }

  # Empty schema: the sink inherits the stream's single json column.
  schema = { fields = [] }

  config = {
    account_id = var.account_id
    bucket     = cloudflare_r2_bucket.cratebank.name
    path       = "raw"

    # Hive-style partitioning is what every query engine expects, so
    # SELECT ... FROM 'https://.../raw/**/*.parquet' prunes by date for free.
    time_pattern = "year=%Y/month=%m/day=%d"

    # Roll on whichever comes first. Five minutes keeps the tail latency
    # low without producing a swarm of tiny objects at low volume.
    interval_seconds = 300
    file_size_bytes  = 104857600 # 100 MiB

    credentials = {
      access_key_id     = var.r2_access_key_id
      secret_access_key = var.r2_secret_access_key
    }
  }
}

# ── the pipeline: stream in, sink out, no transform ──────────────────────────

# SELECT * on purpose. Pipelines SQL has no joins, and our events are
# heterogeneous and correlated by index, so anything that wants rows per
# compilation unit must do that later with a real query engine — where it can
# also be corrected and rerun over everything already collected.
resource "cloudflare_pipeline" "cratebank" {
  account_id = var.account_id
  name       = "cratebank"
  sql        = "INSERT INTO ${cloudflare_pipeline_sink.raw.name} SELECT * FROM ${cloudflare_pipeline_stream.sessions.name}"
}

# ── the name the client ships with ───────────────────────────────────────────

# The stream id is an implementation detail, streams cannot be altered after
# creation so one day we will need a different one, and a released client that
# hardcoded a Cloudflare hostname could never be redirected.
resource "cloudflare_dns_record" "ingest" {
  zone_id = var.zone_id
  name    = "ingest"
  type    = "CNAME"
  content = cloudflare_pipeline_stream.sessions.endpoint
  ttl     = 300
  proxied = false
}
