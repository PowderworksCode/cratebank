# infra

The whole ingest stack as Terraform: one R2 bucket, one stream, one sink, one
pipeline, one DNS record. No server, no application code.

## Apply it

```sh
cp terraform.tfvars.example prod.tfvars   # gitignored; fill it in
terraform init
terraform plan  -var-file=prod.tfvars     # <- the real check, see below
terraform apply -var-file=prod.tfvars
./verify.sh                               # answers the two open questions
```

Leave `zone_id` empty on a first apply. The CNAME is skipped and
`terraform output ingest_endpoint` gives you Cloudflare's raw hostname, so you
can test the pipeline before the domain exists.

### Credentials

Two separate things, which is the usual place to get stuck:

| | where | permissions |
| --- | --- | --- |
| `cloudflare_api_token` | dash → profile → API Tokens | Account · Workers Pipelines · Edit; Account · Workers R2 Storage · Edit; Zone · DNS · Edit (only with `zone_id`) |
| `r2_access_key_id` / `r2_secret_access_key` | dash → R2 → Manage R2 API Tokens | Object Read & Write |

The R2 keys are variables rather than a `cloudflare_api_token` resource on
purpose: creating them in Terraform would write the secret into state.

## `plan` is the real check, not `validate`

`terraform validate` passes against the provider schema, so resource types,
top-level argument names and the `cloudflare_pipeline_stream.sessions.endpoint`
reference are checked. It is **much weaker than it looks for nested blocks**:

> An earlier version of this file put `time_pattern` and `interval_seconds` at
> the top of the sink `config` instead of inside `partitioning` and
> `rolling_policy`. `validate` said "Success!". Adding a nested key called
> `bogus_key = "nonsense"` also passes.

So run `plan` against a real account before believing any of it. That is the
first thing this configuration cannot verify on its own.

## What it creates

| resource | why it looks like that |
| --- | --- |
| `cloudflare_r2_bucket` | storage; zero egress is what makes a public query surface affordable |
| `cloudflare_pipeline_stream` | **unstructured** — one `value` column of arbitrary JSON |
| `cloudflare_pipeline_sink` | R2, parquet, zstd, `year=%Y/month=%m/day=%d`, rolled at 5 min or 100 MiB |
| `cloudflare_pipeline` | `INSERT INTO sink SELECT * FROM stream` — no transform |
| `cloudflare_dns_record` | `ingest.cratebank.io` CNAME to the stream endpoint; skipped if `zone_id` is empty |

Two of those are load-bearing decisions rather than defaults:

**The stream is unstructured on purpose.** Structured streams cannot have their
schema modified after creation, and cargo's log format is still moving — a
declared schema would freeze today's field names into infrastructure we cannot
alter and silently drop every field cargo adds later.

**The pipeline is `SELECT *`.** Pipelines SQL has no joins, and our events are
heterogeneous and correlated by index, so per-unit rows have to be produced
later by a real query engine — which also means that reading can be corrected
and rerun over everything already collected.

## What `verify.sh` settles

Two questions the documentation does not answer, both of which may delete work:

1. **Is `Content-Encoding: br` accepted?** The client sends brotli with no
   fallback. If it is rejected, the client needs one.
2. **Does the 5 MB request limit count compressed or decompressed bytes?** The
   limits page says "5 MB" and never mentions compression. The script posts a
   ~9 MB body that compresses to a few KB: accepted means the limit is on the
   wire, the largest builds are ~260 KB, and **the batching work can be
   dropped**.

It also sends a plain-JSON control first, so a failure is attributable rather
than mysterious.

Unverified until an apply happens: `format.unstructured = true` on the sink is
the documented flag for pass-through of an arbitrary-JSON column, but its
behaviour with a `value`-only stream has not been observed.

## Pulumi

`pulumi-cloudflare` (v6.19.0) is bridged from this same Terraform provider, so
the resources and arguments are identical modulo naming convention:

```sh
pulumi convert --from terraform --language typescript --out ../infra-pulumi
```

That is the honest recommendation over hand-writing it — the bridge means the
Terraform is the source of truth for what the provider accepts either way, and
`convert` will not invent an argument the provider does not have.
