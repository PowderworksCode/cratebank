// cratebank ingest: take a blob, put it in R2. That is the whole program.
//
// It does not decompress, parse, validate or interpret the body. The client
// uploads a zstd-compressed session and those exact bytes land in R2. The
// compactor reads those objects to produce the public parquet tables.
// The request ceiling is the zone's request-body size (100 MB on Free/Pro).

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

    // A browser landing here is a person, not a client. They typed or clicked
    // the hostname printed in `cargo cratebank status`, and a JSON 405 tells
    // them nothing about what this is. Send them to the page that does.
    if (request.method === "GET" || request.method === "HEAD") {
      return Response.redirect("https://cratebank.io/", 302);
    }
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
      // Report the failure rather than swallowing it. The client exits with an
      // error unless the upload receives a successful response.
      return json({ success: false, error: `r2 put failed: ${e}` }, 500);
    }

    return json({ success: true, key });
  },
};
