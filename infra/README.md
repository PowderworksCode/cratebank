# infra

The whole cratebank stack: Terraform owns the R2 data bucket, two request-time
Workers, domains, and the compaction cron; Wrangler uploads the static landing
page. Merges to `main` deploy both through `.github/workflows/deploy.yml`, so a
developer laptop is not part of the ongoing deployment path.

## GitHub deployment

The workflow renders the landing page, builds the compaction Worker, uploads
the page as Workers Static Assets, makes an OpenTofu plan, and applies that
exact saved plan. The `production` environment and workflow concurrency provide
the approval and serialization boundaries.

Create a private R2 bucket named `cratebank-tofu-state`. Do not add a custom
domain to it and do not reuse the public `cratebank` data bucket: OpenTofu state
contains the manual compaction secret. Create an R2 Object Read & Write token
scoped to this state bucket.

Configure a GitHub environment named `production` with:

| kind | name | value |
| --- | --- | --- |
| variable | `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id |
| variable | `CLOUDFLARE_ZONE_ID` | `cratebank.io` zone id |
| secret | `CLOUDFLARE_API_TOKEN` | token described below |
| secret | `COMPACT_SECRET` | existing bearer token for `POST /compact` |
| secret | `TOFU_STATE_ACCESS_KEY_ID` | state-bucket-scoped R2 access key |
| secret | `TOFU_STATE_SECRET_ACCESS_KEY` | matching R2 secret key |

Before merging the workflow, migrate the existing local state once from the
checkout that currently owns it:

```sh
export AWS_ACCESS_KEY_ID='<state bucket access key>'
export AWS_SECRET_ACCESS_KEY='<state bucket secret key>'
export AWS_ENDPOINT_URL_S3='https://<account id>.r2.cloudflarestorage.com'

tofu init -migrate-state -backend-config='bucket=cratebank-tofu-state'
tofu plan -var-file=prod.tfvars
```

After that migration, deploy by merging to `main` or by running the `deploy`
workflow manually. Do not restore a local backend or run concurrent local
applies; GitHub is the production writer. Local plans still work with the same
three `AWS_*` variables and `-backend-config` argument.

## OpenTofu, not Terraform

`required_version >= 1.6` cannot be met by Homebrew's `terraform`, which is
frozen at 1.5.7 and marked deprecated because the next release changed to BUSL.
Use `brew install opentofu` and run `tofu`. The config is plain HCL with nothing
HashiCorp-specific in it, so either binary works if you have a recent one.

### Cloudflare credentials

Two separate things, which is the usual place to get stuck:

| | where | permissions |
| --- | --- | --- |
| `cloudflare_api_token` | dash → profile → API Tokens | Account · Workers Scripts · Edit; Account · Workers R2 Storage · Edit; Zone · Workers Routes · Edit; Zone · DNS · Edit |
| state R2 access key / secret | dash → R2 object storage → **Account Details** panel → **Manage** next to **API Tokens** | Object Read & Write, scoped to `cratebank-tofu-state` |

Adding any Zone-level row makes a **Zone Resources** selector appear below
Account Resources. It defaults to *All zones* — set it to *Include →
cratebank.io* unless you want a token in a `.env` that can rewrite DNS for every
domain on the account.

The R2 state keys are not used by a Worker. Workers reach the public data
bucket through bindings and need no storage credential.

Keep the state token bucket-scoped. Anyone who can read the state object can
read sensitive Worker bindings even though GitHub masks the original secret.

## `plan` is the real check, not `validate`

`tofu validate` passes against the provider schema, so resource types and
top-level argument names are checked. It is **much weaker than it looks for
nested blocks**:

> An earlier version of this file put `time_pattern` and `interval_seconds` at
> the top of a sink `config` instead of inside `partitioning` and
> `rolling_policy`. `validate` said "Success!". Adding a nested key called
> `bogus_key = "nonsense"` also passes.

So run `plan` against a real account before believing any of it.

## What deployment creates

| resource | why it looks like that |
| --- | --- |
| `cloudflare_r2_bucket` | storage; zero egress is what makes a public query surface affordable |
| `cloudflare_workers_script.ingest` | the ingest Worker, with an R2 binding named `BUCKET` |
| `cloudflare_workers_custom_domain.ingest` | `ingest.cratebank.io` → the Worker; skipped if `zone_id` is empty |
| `cloudflare_workers_script.compact` | nightly compaction to public parquet (`compact.tf`) |
| `cloudflare_workers_cron_trigger.compact` | `0 5 * * *` |
| `cloudflare_workers_script_subdomain.compact` | workers.dev route for the manual `POST /compact` trigger |
| `cloudflare_r2_custom_domain.data` | `data.cratebank.io` → the bucket, **public** |
| Wrangler `cratebank-site` assets | rendered landing page, served without request-time Worker code |

**The Worker never decodes the body.** It streams `request.body` straight to
`env.BUCKET.put()`. It requires `content-length` and returns 411 without it, so
a 100 MB upload never has to be held in the Worker's 128 MB of memory.

**Do not add a `cloudflare_dns_record` for `ingest` or `data`.** A Workers
custom domain and an R2 custom domain each create and manage their own DNS
record; declaring one alongside makes them fight over the same hostname.

**`data.cratebank.io` makes the entire bucket world-readable**, raw session
blobs included. That is intended — the census is public — but it is not
reversible for anything already fetched or indexed.

## Worker sources and bundles

The editable page lives in `worker/site.md`, with presentation in
`worker/site.css` and the document shell in `worker/site.template.html`.
`build-site.mjs` renders it once to `worker/dist/site/index.html`; Wrangler then
uploads that directory using `worker/wrangler-site.jsonc`. There is no site
Worker source and no Markdown rendering on a request.

`worker/dist/compact.js` is the committed bundle Terraform deploys. CI rebuilds
it and fails on drift. The rendered site directory is ignored and rebuilt in
GitHub immediately before upload.

Rebuild after editing any Worker source:

```sh
cd worker && npm ci && npm run build
```

## The compaction Worker

`worker/compact.js` reads every session blob nightly and writes
`units.parquet` and `sessions.parquet` to the bucket root, plus dated copies
under `snapshots/`. Those are the public interface; the raw blobs are the
ground truth.

Trigger it by hand rather than waiting for 05:00 UTC:

```sh
curl -X POST "https://cratebank-compact.<subdomain>.workers.dev/compact" \
  -H "authorization: Bearer $compact_secret"
```

It is a full rebuild holding every row in memory, which is free now and will
not be — the ceiling is the Worker's 128 MB, roughly 10k sessions. Watch
`objects` and `bytes_in` in the cron logs; the fix when it arrives is per-day
compaction, which the hive key layout already supports.

## Two traps, both the same shape

Terraform reads a server-populated field as a field you deleted, and plans a
change forever. Both were caught by reading a plan that claimed to modify
something nobody had touched.

**Optional+computed blocks must be declared in full.** `observability = {
enabled = true }` looks complete, but the server fills in `head_sampling_rate`,
`logs` and `traces`. Terraform then reads the difference as a removal and
re-uploads the script on **every plan**. Declare every sub-field, or expect
permanent drift.

**The same bug bites harder on resources that cannot be updated in place.** The
now-removed Pipelines stream had a server-generated schema; the config did not
declare it, so every plan forced a *replacement*, minting a new stream id and
endpoint each time. It needed `lifecycle { ignore_changes = [schema] }`. If a
plan proposes replacing something you did not touch, read the diff before
applying it.

## What was here before

A Cloudflare Pipelines stream, sink and pipeline, deleted once the Worker
existed. `docs/ingest.md` records why in detail; the short version is a 1 MB
per-message cap that appears in no documentation, and an ingest that answered
`200 committed:N` for events it then silently discarded.

Two findings worth keeping if anyone reconsiders Pipelines:

- **`format.unstructured = true` is the only way to get an unstructured
  stream.** Declaring `schema.fields = [{name="value", type="json", required=true}]`
  instead produces a *structured* stream, and every event without a literal
  `value` key — which is every event this client sends — is accepted and then
  dropped before reaching R2.
- **Deleting a stream requires destroying the pipeline first**, and Terraform
  will not work that out if the pipeline's SQL string is unchanged. It fails
  mid-apply with `422 Stream still in use and cannot be deleted`; force it with
  `tofu apply -replace=cloudflare_pipeline.<name>`.

## Pulumi

`pulumi-cloudflare` (v6.19.0) is bridged from this same Terraform provider, so
the resources and arguments are identical modulo naming convention:

```sh
pulumi convert --from terraform --language typescript --out ../infra-pulumi
```

That is the honest recommendation over hand-writing it — the bridge means the
Terraform is the source of truth for what the provider accepts either way, and
`convert` will not invent an argument the provider does not have.
