// cratebank ingest: take a blob, put it in R2. That is the whole program.
//
// It does not decompress, parse, validate or interpret the body. The client
// uploads a zstd-compressed session and those exact bytes land in R2, where
// DuckDB reads them natively -- so there is no conversion step between
// contribution and query, and nothing here can drop a row by misunderstanding
// it. "Capture generously, model nothing at ingest", enforced by having no
// code that could do otherwise.
//
// This replaced a Cloudflare Pipelines stream, which capped requests at 5 MB
// and each message at 1 MB. A Worker's limit is the plan's request-body size
// (100 MB on Free/Pro), which removes the need for client-side batching.

const PATH = "/v1/sessions";

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

const pad = (n) => String(n).padStart(2, "0");

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method !== "POST") {
      return json({ success: false, error: "POST only" }, 405);
    }
    if (url.pathname !== PATH) {
      return json({ success: false, error: `not found; POST to ${PATH}` }, 404);
    }

    // R2 needs to know the length up front to store a stream without
    // buffering it. Requiring it means a 100 MB upload never has to be held
    // in the Worker's 128 MB of memory, so the body streams straight through.
    const length = request.headers.get("content-length");
    if (length === null) {
      return json({ success: false, error: "content-length required" }, 411);
    }
    if (Number(length) === 0) {
      return json({ success: false, error: "empty body" }, 400);
    }

    // Hive-style partitioning, so a query engine prunes by date for free:
    //   SELECT * FROM read_json_auto('.../sessions/**/*.json.zst')
    const now = new Date();
    const key =
      `sessions/year=${now.getUTCFullYear()}` +
      `/month=${pad(now.getUTCMonth() + 1)}` +
      `/day=${pad(now.getUTCDate())}` +
      `/${crypto.randomUUID()}.json.zst`;

    try {
      await env.BUCKET.put(key, request.body, {
        httpMetadata: { contentType: "application/zstd" },
      });
    } catch (e) {
      // Report the failure rather than swallowing it: the client only marks a
      // session sent on a 2xx, so a 5xx means it stays queued and retries.
      return json({ success: false, error: `r2 put failed: ${e}` }, 500);
    }

    return json({ success: true, key });
  },
};
