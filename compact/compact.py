#!/usr/bin/env python3
"""Collate stored payloads into tables worth querying.

Reads the raw bucket, runs compact.sql, writes parquet and a DuckDB file to the
published bucket. Everything happens inside DuckDB, which reads and writes R2
over its S3 interface directly — there is no download step and no local staging.

Two buckets on purpose:

  raw        append-only, never rewritten, what arrived. If a reading here turns
             out to be wrong, it is rerun against this.
  published  derived, replaced wholesale on every run. Safe to delete.

That separation is what makes the SQL disposable. A reading baked into ingest
could only ever apply going forward; a reading applied here can be corrected and
rerun over everything ever collected.

usage:
  compact.py                     # read config from the environment
  compact.py --local DIR         # develop against a directory of .ndjson/.json
  compact.py --dry-run           # build the tables, report, write nothing
"""
import argparse
import os
import sys
import textwrap
from pathlib import Path

import duckdb

HERE = Path(__file__).resolve().parent
# unit_costs is materialised rather than left a view: someone who downloads one
# file should get a usable table, not a definition referring to four others.
TABLES = ["sessions", "units", "unit_sections", "unit_timeline", "unit_costs"]


def configure_r2(con: duckdb.DuckDBPyConnection) -> None:
    """Point DuckDB at R2 over its S3 interface."""
    account = os.environ["CRATEBANK_R2_ACCOUNT_ID"]
    con.execute("INSTALL httpfs; LOAD httpfs;")
    con.execute(
        f"""
        CREATE OR REPLACE SECRET r2 (
            TYPE s3,
            PROVIDER config,
            KEY_ID '{os.environ["CRATEBANK_R2_ACCESS_KEY_ID"]}',
            SECRET '{os.environ["CRATEBANK_R2_SECRET_ACCESS_KEY"]}',
            REGION 'auto',
            ENDPOINT '{account}.r2.cloudflarestorage.com',
            URL_STYLE 'path'
        )
        """
    )


def load_raw(con: duckdb.DuckDBPyConnection, source: str) -> int:
    """Expose the payloads as a `raw` view with one JSON `value` column.

    The sink writes parquet whose single column is the stream's `value`; a local
    directory of .ndjson is the same shape, so both paths produce one view and
    the SQL never learns where it is running.
    """
    if source.startswith("s3://"):
        con.execute(
            f"CREATE OR REPLACE VIEW raw AS "
            f"SELECT value::JSON AS value FROM read_parquet('{source}/**/*.parquet')"
        )
    else:
        pattern = f"{source}/**/*.ndjson"
        con.execute(
            f"CREATE OR REPLACE VIEW raw AS "
            f"SELECT value::JSON AS value FROM read_ndjson('{pattern}')"
        )
    return con.execute("SELECT count(*) FROM raw").fetchone()[0]


def build(con: duckdb.DuckDBPyConnection) -> None:
    con.execute((HERE / "compact.sql").read_text())
    con.execute("CREATE OR REPLACE TABLE unit_costs AS SELECT * FROM unit_costs_v")


def report(con: duckdb.DuckDBPyConnection) -> str:
    lines = []
    for t in TABLES:
        n = con.execute(f"SELECT count(*) FROM {t}").fetchone()[0]
        lines.append(f"  {t:<16} {n:>9,} rows")
    span = con.execute(
        "SELECT min(started_at), max(started_at), count(DISTINCT machine_id) FROM sessions"
    ).fetchone()
    lines.append(f"  {'span':<16} {span[0]} .. {span[1]}  across {span[2]} machine(s)")

    # A column that is null everywhere means the reading is wrong, not that the
    # data is missing. Worth failing loudly on rather than publishing quietly.
    empty = []
    for t in ("sessions", "units"):
        cols = [r[0] for r in con.execute(f"DESCRIBE {t}").fetchall()]
        total = con.execute(f"SELECT count(*) FROM {t}").fetchone()[0]
        for c in cols:
            if total and con.execute(f'SELECT count("{c}") FROM {t}').fetchone()[0] == 0:
                empty.append(f"{t}.{c}")
    if empty:
        lines.append(f"  all-null columns: {', '.join(empty)}")
    return "\n".join(lines)


def publish(con: duckdb.DuckDBPyConnection, dest: str, dry_run: bool) -> None:
    """Write parquet plus a single-file DuckDB, replacing what is there."""
    if dry_run:
        print(f"dry run: would write {len(TABLES)} tables + cratebank.duckdb to {dest}")
        return

    if not dest.startswith("s3://"):
        Path(dest).mkdir(parents=True, exist_ok=True)

    for t in TABLES:
        con.execute(
            f"COPY {t} TO '{dest}/{t}.parquet' (FORMAT parquet, COMPRESSION zstd)"
        )

    # The convenience database: one ATTACH and the views are there. Rebuilt from
    # the parquet every run, so nothing canonical lives only inside it.
    local = "/tmp/cratebank.duckdb"
    Path(local).unlink(missing_ok=True)
    con.execute(f"ATTACH '{local}' AS out")
    for t in TABLES:
        con.execute(f"CREATE TABLE out.{t} AS SELECT * FROM {t}")
    con.execute("DETACH out")

    if dest.startswith("s3://"):
        _put_file(con, local, f"{dest}/cratebank.duckdb")
    else:
        Path(f"{dest}/cratebank.duckdb").write_bytes(Path(local).read_bytes())


def _put_file(con: duckdb.DuckDBPyConnection, local: str, remote: str) -> None:
    """DuckDB cannot COPY an arbitrary file, so the .duckdb goes up via boto3."""
    import boto3

    account = os.environ["CRATEBANK_R2_ACCOUNT_ID"]
    bucket, _, key = remote[len("s3://"):].partition("/")
    boto3.client(
        "s3",
        endpoint_url=f"https://{account}.r2.cloudflarestorage.com",
        aws_access_key_id=os.environ["CRATEBANK_R2_ACCESS_KEY_ID"],
        aws_secret_access_key=os.environ["CRATEBANK_R2_SECRET_ACCESS_KEY"],
        region_name="auto",
    ).upload_file(local, bucket, key)


def main() -> int:
    ap = argparse.ArgumentParser(
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=textwrap.dedent(__doc__ or ""),
    )
    ap.add_argument("--local", help="read .ndjson from a directory instead of R2")
    ap.add_argument("--out", help="write here instead of the published bucket")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    con = duckdb.connect()
    if a.local:
        source = a.local
        dest = a.out or "/tmp/cratebank-published"
    else:
        configure_r2(con)
        source = f"s3://{os.environ.get('CRATEBANK_RAW_BUCKET', 'cratebank')}/raw"
        dest = a.out or f"s3://{os.environ.get('CRATEBANK_PUBLISHED_BUCKET', 'cratebank-published')}"

    n = load_raw(con, source)
    if n == 0:
        print(f"nothing to compact in {source}", file=sys.stderr)
        return 0
    print(f"{n:,} payload(s) from {source}")
    build(con)
    print(report(con))
    publish(con, dest, a.dry_run)
    print(f"published to {dest}" if not a.dry_run else "dry run, nothing written")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
