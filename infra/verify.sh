#!/usr/bin/env bash
# Answers the two questions the design could not settle from documentation.
# Run after `terraform apply`.
set -uo pipefail

ENDPOINT="${1:-$(terraform output -raw ingest_endpoint 2>/dev/null)}"
[ -z "$ENDPOINT" ] && { echo "usage: ./verify.sh https://ingest.example.com"; exit 2; }
echo "endpoint: $ENDPOINT"
code() { curl -s -o /tmp/cb-resp -w '%{http_code}' "$@"; }

echo
echo "1. plain JSON — the control. Anything other than 2xx means the rest is noise."
c=$(code -X POST "$ENDPOINT" -H 'content-type: application/json' -d '[{"probe":"plain"}]')
echo "   HTTP $c  $(head -c 120 /tmp/cb-resp)"

echo
echo "2. Content-Encoding: br — the client sends this with no fallback."
if command -v brotli >/dev/null; then
  printf '[{"probe":"brotli"}]' | brotli -c > /tmp/cb.br
  c=$(code -X POST "$ENDPOINT" -H 'content-type: application/json' \
        -H 'content-encoding: br' --data-binary @/tmp/cb.br)
  echo "   HTTP $c  $(head -c 120 /tmp/cb-resp)"
  echo "   -> 2xx means brotli is accepted. Anything else and the client needs a fallback."
else
  echo "   (skipped: install brotli)"
fi

echo
echo "3. Does the 5 MB limit count compressed or decompressed bytes?"
echo "   A ~9 MB body that compresses small. Accepted => the limit is on the wire,"
echo "   so the largest builds are ~260 KB and no batching is needed."
if command -v brotli >/dev/null; then
  /home/exedev/sessions/buildprof/.venv/bin/python3 - > /tmp/cb-big.json <<'PY' 2>/dev/null || python3 - > /tmp/cb-big.json <<'PY'
import json
print(json.dumps([{"probe": "size", "pad": "x" * 9_000_000}]))
PY
  brotli -c < /tmp/cb-big.json > /tmp/cb-big.br
  echo "   raw $(wc -c < /tmp/cb-big.json) B -> brotli $(wc -c < /tmp/cb-big.br) B"
  c=$(code -X POST "$ENDPOINT" -H 'content-type: application/json' \
        -H 'content-encoding: br' --data-binary @/tmp/cb-big.br)
  echo "   HTTP $c  $(head -c 120 /tmp/cb-resp)"
  echo "   -> 2xx: limit is compressed bytes (drop the batching work)."
  echo "      413/400: limit is decompressed bytes (batching stays)."
fi

echo
echo "4. Did anything land? Give the sink its roll interval (300s) first."
echo "   Then:  npx wrangler r2 object list $(terraform output -raw bucket 2>/dev/null || echo cratebank) --prefix raw/"
