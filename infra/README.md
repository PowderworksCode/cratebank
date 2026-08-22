# infra

The whole cratebank stack: OpenTofu owns the R2 data bucket, two request-time
Workers, domains, and the compaction cron; Wrangler uploads the static landing
page. Merges to `main` deploy the stack through `.github/workflows/deploy.yml`.

## GitHub deployment

The workflow renders the landing page, builds the compaction Worker, uploads
the page as Workers Static Assets, makes an OpenTofu plan, and applies that
exact saved plan. The `production` environment and workflow concurrency provide
the approval and serialization boundaries.

Approved pull requests also use the `preview` environment to:

- deploy the rendered page to a disposable `cratebank-site-pr-<number>`
  workers.dev URL;
- make a speculative OpenTofu plan against the private R2 state; and
- maintain one PR comment containing the preview link and redacted plan output.

PRs authored by `zmaril` run automatically. For every other author, `zmaril`
must use the exact `/preview <full-head-sha>` command that the bot posts; that
binds approval to one revision, so a later push needs another approval. The
privileged workflow is loaded from `main` with `pull_request_target` or
`issue_comment`, which prevents PR code from weakening its own gate. It also
checks the approved SHA is still current before executing PR code. Closing or
merging the PR deletes its preview Worker. No binary PR plan is retained: plan
files can contain cleartext secrets even when the CLI output hides them.

OpenTofu state is stored in the private `cratebank-tofu-state` R2 bucket. The
bucket has no custom domain and is separate from the public `cratebank` data
bucket because state contains the manual compaction secret. Its access token is
scoped to Object Read & Write on that bucket.

Configure a GitHub environment named `production` with:

| kind | name | value |
| --- | --- | --- |
| variable | `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id |
| variable | `CLOUDFLARE_ZONE_ID` | `cratebank.io` zone id |
| secret | `CLOUDFLARE_API_TOKEN` | token described below |
| secret | `COMPACT_SECRET` | existing bearer token for `POST /compact` |
| secret | `TOFU_STATE_ACCESS_KEY_ID` | state-bucket-scoped R2 access key |
| secret | `TOFU_STATE_SECRET_ACCESS_KEY` | matching R2 secret key |

The `preview` environment has the same variables and secrets. Both environments
are restricted to `main`: the trusted preview workflow runs from the default
branch and checks out only the approved PR revision.

A pull request gets a static preview and a speculative plan according to the
approval policy above. Merge to `main`, or run the `deploy` workflow manually,
to deploy production. The production job creates and applies a fresh saved
plan. GitHub Actions is the production state writer; do not run concurrent
local applies.

For a read-only local plan, export `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, and `AWS_ENDPOINT_URL_S3`, then initialize with:

```sh
tofu init -backend-config='bucket=cratebank-tofu-state'
tofu plan
```

## OpenTofu

Use the OpenTofu version recorded in `.opentofu-version`. On macOS, install it
with `brew install opentofu` and run it as `tofu`.

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

`tofu validate` checks resource types and top-level argument names but does not
reliably validate every provider-specific nested block. Treat the remote-state
`plan` against the Cloudflare account as the authoritative configuration check.

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
`sessions.parquet`, `units.parquet`, `phases.parquet`, `timeline.parquet`, and
`unit_flags.parquet` to the bucket root, plus dated copies under `snapshots/`.
Those files are the public interface; the raw blobs are the ground truth.

Trigger it by hand rather than waiting for 05:00 UTC:

```sh
curl -X POST "https://cratebank-compact.<subdomain>.workers.dev/compact" \
  -H "authorization: Bearer $compact_secret"
```

It is a full rebuild holding every row in memory, which is free now and will
not be — the ceiling is the Worker's 128 MB, roughly 10k sessions. Watch
`objects` and `bytes_in` in the cron logs; the fix when it arrives is per-day
compaction, which the hive key layout already supports.

## Provider drift

Declare Cloudflare optional-and-computed blocks in full. For example, the
Workers `observability` block includes `head_sampling_rate`, `logs`, and
`traces` alongside `enabled`. Omitting server-populated fields creates
permanent plan drift and repeated script uploads. Review any unexpected
replacement in the speculative plan before merging.
