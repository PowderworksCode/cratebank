// cratebank.io — the landing page, and a redirect from data.cratebank.io.
//
// One Worker, no build step, no assets. The page is small enough to inline,
// and inlining means there is nothing to keep in sync and nothing to fetch.

const SITE = `<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>cratebank — a census of Rust build times</title>
<meta name="description" content="Share the Rust build timings you were already producing. An opt-in, public census of where compile time actually goes.">
<style>
  /* The tokens. Every literal colour and font in the page is defined here and
     referenced as a variable below, so a change lands in one place. The two
     palette lines are the only ones that ask straitjacket for an exemption:
     a token definition is where the literal is supposed to be, and the marker
     is scoped to that line so a stray colour further down still fails. */
  :root {
    --font-body: ui-serif, Georgia, "Times New Roman", serif;
    --font-mono: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  :root { color-scheme: light dark; --fg:#111; --dim:#555; --bg:#fdfdfc; --line:#e3e3e0; --code:#f4f4f1; --accent:#a4432b; } /* straitjacket-allow:color */
  @media (prefers-color-scheme: dark) {
    :root { --fg:#e8e8e6; --dim:#a0a09b; --bg:#16161a; --line:#2c2c31; --code:#1e1e23; --accent:#e08a68; } /* straitjacket-allow:color */
  }
  * { box-sizing: border-box; }
  body { margin:0; background:var(--bg); color:var(--fg);
         font:16px/1.65 var(--font-body); }
  main { max-width:46rem; margin:0 auto; padding:3.5rem 1.25rem 5rem; }
  h1 { font-size:2rem; margin:0 0 .25rem; letter-spacing:-.02em; }
  .tag { color:var(--dim); margin:0 0 2.5rem; font-size:1.05rem; }
  h2 { font-size:1.15rem; margin:2.5rem 0 .6rem; letter-spacing:-.01em; }
  p, li { margin:.6rem 0; }
  code, pre { font-family:var(--font-mono); font-size:.875rem; }
  pre { background:var(--code); border:1px solid var(--line); border-radius:6px;
        padding:.85rem 1rem; overflow-x:auto; line-height:1.5; }
  code:not(pre code) { background:var(--code); padding:.1rem .3rem; border-radius:3px; }
  a { color:var(--accent); }
  ul { padding-left:1.1rem; }
  hr { border:0; border-top:1px solid var(--line); margin:2.5rem 0; }
  footer { color:var(--dim); font-size:.9rem; }
  .note { color:var(--dim); font-size:.92rem; }
</style>
<main>
  <h1>cratebank</h1>
  <p class="tag">A public census of where Rust build time actually goes.</p>

  <h2>Why</h2>
  <p>
    Everyone has a theory about what makes Rust builds slow — generics, macros,
    LLVM, linking. Very little of it is measured, and almost none of it is
    measured across the enormous range of machines and crate graphs that real
    builds happen on.
  </p>
  <p>
    cratebank collects the timings your builds already produce, plus a sampled
    breakdown of what the compiler was doing, and publishes the result so
    anyone can query it. The same dependencies get compiled on thousands of
    machines; that overlap is what makes the comparison work.
  </p>

  <h2>Getting started</h2>
  <pre>cargo install cargo-cratebank
cargo install samply          # the profiler that measures compiler phases

cargo cratebank build         # builds, measures, and sends</pre>
  <p class="note">
    Needs a nightly toolchain for cargo's build-analysis logs. Without samply
    it still works — you get the build and the timings, just no phase
    breakdown.
  </p>

  <h2>What gets sent</h2>
  <ul>
    <li><strong>Public crates only.</strong> Anything not from crates.io or a
      public git remote is dropped entirely — not the name, not a hash, not a
      timing. Only a count of how many units were withheld survives, so the
      graph is honestly marked as partial.</li>
    <li><strong>No paths, ever.</strong> Working directory, target directory
      and manifest path are stripped; compiler flags are kept but their path
      values are not.</li>
    <li><strong>Your own crates are private by default.</strong> Publishing
      them takes an explicit opt-in:
      <code>[package.metadata.cratebank] public = true</code>.</li>
  </ul>
  <p class="note">
    Nothing sits in your compile path, so there is no conflict with
    <code>sccache</code> or any other <code>RUSTC_WRAPPER</code>, and no build
    is ever run on your behalf.
  </p>

  <h2>Using the data</h2>
  <p>
    Everything is public parquet on R2 — no account, no API key, no egress
    cost. Query it straight from <a href="https://duckdb.org">DuckDB</a>:
  </p>
  <pre>INSTALL httpfs; LOAD httpfs;

-- where does a crate's compile time go?
SELECT package, phase, sum(samples) AS samples
FROM 'https://data.cratebank.io/phases.parquet'
WHERE thread = 'serial'
GROUP BY 1, 2 ORDER BY samples DESC;</pre>
  <p>Five tables, described by a machine-readable schema:</p>
  <ul>
    <li><code>sessions.parquet</code> — one row per build</li>
    <li><code>units.parquet</code> — one row per compilation unit</li>
    <li><code>phases.parquet</code> — sampled compiler phases per unit</li>
    <li><code>timeline.parquet</code> — build concurrency and CPU over time</li>
    <li><code>unit_flags.parquet</code> — the settings each unit was built with</li>
  </ul>
  <p>
    <a href="https://data.cratebank.io/schema/v1/tables.json">schema/v1/tables.json</a>
    describes every column, and carries the warnings that matter — chiefly that
    sampled phases are <em>CPU</em> while the section boundaries in
    <code>units</code> are <em>wall clock</em>, and the two are not
    interchangeable.
  </p>

  <hr>
  <footer>
    <a href="https://github.com/PowderworksCode/cratebank">Source on GitHub</a>
    · opt-in · public domain data
  </footer>
</main>
</html>`;

export default {
  async fetch(request) {
    const url = new URL(request.url);

    // data.cratebank.io is the R2 bucket; only its bare root reaches this
    // Worker, and a 404 there is unhelpful when someone types the hostname
    // they saw in a query.
    if (url.hostname.startsWith("data.")) {
      return Response.redirect("https://cratebank.io/", 302);
    }

    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("GET only", { status: 405 });
    }

    return new Response(SITE, {
      headers: {
        "content-type": "text/html; charset=utf-8",
        // Short: the page changes with the project, and it is one document.
        "cache-control": "public, max-age=300",
      },
    });
  },
};
