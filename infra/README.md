# infra

The whole ingest stack as Terraform: one R2 bucket, one stream, one sink, one
pipeline, one DNS record. No server, no application code.

```sh
cd infra
terraform init
terraform plan  -var-file=prod.tfvars
terraform apply -var-file=prod.tfvars
```

`terraform validate` passes against the real provider schema (cloudflare
~> 5.23), so the resource types, argument names and the
`cloudflare_pipeline_stream.sessions.endpoint` reference are checked rather than
assumed. What it cannot check is behaviour: run `plan` against the account
before believing any of it.

## What it creates

| resource | why it looks like that |
| --- | --- |
| `cloudflare_r2_bucket` | storage; zero egress is what makes a public query surface affordable |
| `cloudflare_pipeline_stream` | **unstructured** — one `value` column of arbitrary JSON |
| `cloudflare_pipeline_sink` | R2, parquet, zstd, `year=%Y/month=%m/day=%d`, rolled at 5 min or 100 MiB |
| `cloudflare_pipeline` | `INSERT INTO sink SELECT * FROM stream` — no transform |
| `cloudflare_dns_record` | `ingest.cratebank.io` CNAME to the stream's endpoint |

Two of those are load-bearing decisions rather than defaults:

**The stream is unstructured on purpose.** Structured streams cannot have their
schema modified after creation, and cargo's log format is still moving — a
declared schema would freeze today's field names into infrastructure we cannot
alter and silently drop every field cargo adds later.

**The pipeline is `SELECT *`.** Pipelines SQL has no joins, and our events are
heterogeneous and correlated by index, so per-unit rows have to be produced
later by a real query engine — which also means that reading can be corrected
and rerun over everything already collected.

## Credentials

`prod.tfvars` (gitignored) holds the account id, zone id, an API token with
Pipelines + R2 + DNS edit, and an R2 access key pair for the sink.

The R2 keys are variables rather than a `cloudflare_api_token` resource on
purpose: creating them in Terraform would write the secret into state.

## Pulumi

`pulumi-cloudflare` (v6.19.0) is bridged from this same Terraform provider, so
the resources and arguments are identical modulo naming convention. If you would
rather have Pulumi:

```sh
pulumi convert --from terraform --language typescript --out ../infra-pulumi
```

That is the honest recommendation over hand-writing it — the bridge means the
Terraform is the source of truth for what the provider accepts either way, and
`pulumi convert` will not invent an argument the provider does not have.

## Verifying the two open questions

Both are one request each against the live endpoint, and both may delete work:

1. **Does the endpoint accept `Content-Encoding: br`?** The client sends brotli
   with no fallback.
2. **Does the 5 MB request limit count compressed or decompressed bytes?** If
   compressed, the largest builds land near 260 KB and no batching is needed.

```sh
printf '[{"hello":"world"}]' | brotli -c > /tmp/t.br
curl -sv -X POST https://ingest.cratebank.io \
  -H 'content-type: application/json' -H 'content-encoding: br' \
  --data-binary @/tmp/t.br
```
