#!/usr/bin/env python3
"""Fetch/verify/import the pinned real-coordinate NYC January 2015 corpus.

No network or database access at import time. Fetch is checksum-enforced; import
verifies the entire source before opening a transaction and never overwrites an
existing table. Data dependencies are explicit, outside the default test suite.
"""

import argparse
import bz2
import csv
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks/corpora/nyc-yellow-2015-01/manifest.json"
CACHE = ROOT / ".pgaccel/corpora/nyc-yellow-2015-01/source.csv"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class CopyDigestReader:
    """Hash the exact binary chunks handed to COPY, not a later path reopening."""

    def __init__(self, source):
        self.source = source
        self.digest = hashlib.sha256()

    def read(self, size=-1):
        block = self.source.read(size)
        self.digest.update(block)
        return block


def publish_noreplace(partial, destination):
    """Publish a sibling partial atomically without replacing a competing file.

    A hard link fails if the destination already exists. Do not fall back to
    rename, which can silently overwrite another writer's completed download.
    Keep the partial for diagnosis if publication fails.
    """
    os.link(partial, destination)
    partial.unlink()


def read_manifest(path=MANIFEST, *, candidate=False):
    manifest = json.loads(path.read_text())
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported corpus manifest")
    if not candidate and not re.fullmatch(r"[a-f0-9]{64}", manifest.get("source_sha256", "")):
        raise ValueError("corpus source must have a reviewed pinned SHA-256")
    if not candidate and "archive" in manifest and not re.fullmatch(
            r"[a-f0-9]{64}", manifest["archive"].get("sha256", "")):
        raise ValueError("corpus archive must have a reviewed pinned SHA-256")
    return manifest


def verify_source(path, manifest):
    if path.stat().st_size > manifest["max_download_bytes"]:
        raise ValueError("corpus exceeds download byte limit")
    if sha256(path) != manifest["source_sha256"]:
        raise ValueError("source SHA-256 mismatch; do not silently repin upstream changes")
    with path.open(newline="", encoding="utf-8") as source:
        reader = csv.reader(source, strict=True)
        if next(reader, None) != manifest["columns"]:
            raise ValueError("unexpected corpus CSV header")
        rows = 0
        for row in reader:
            if len(row) != len(manifest["columns"]) or not row[0]:
                raise ValueError(f"malformed corpus row {rows + 1}")
            rows += 1
    if rows != manifest["rows"]:
        raise ValueError(f"expected {manifest['rows']} source rows, found {rows}")
    return rows


def download_archive(path, manifest):
    if path.exists():
        if sha256(path) != manifest["archive"]["sha256"]:
            raise ValueError("archive SHA-256 mismatch")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    partial = path.with_suffix(path.suffix + ".partial")
    request = urllib.request.Request(manifest["archive"]["url"],
                                     headers={"User-Agent": "pg_accel-corpus/1"})
    with partial.open("xb") as output, urllib.request.urlopen(request, timeout=180) as source:
        downloaded = 0
        for block in iter(lambda: source.read(1024 * 1024), b""):
            downloaded += len(block)
            if downloaded > manifest["archive"]["max_download_bytes"]:
                raise ValueError("archive exceeds download byte limit")
            output.write(block)
    if sha256(partial) != manifest["archive"]["sha256"]:
        raise ValueError("archive SHA-256 mismatch; partial retained")
    publish_noreplace(partial, path)


def project_archive(archive, path, manifest, *, candidate=False):
    """Project every original CSV row, preserving source order and decimal text.

    Only source columns are selected; no records are filtered, deduplicated,
    imputed, rounded or repeated. A unique source ordinal identifies each row.
    Candidate output cannot be imported until its manifest pins are reviewed.
    """
    if path.exists():
        raise ValueError("refusing to replace existing corpus")
    if archive.stat().st_size > manifest["archive"]["max_download_bytes"]:
        raise ValueError("archive exceeds download byte limit")
    digest = sha256(archive)
    if not candidate and digest != manifest["archive"]["sha256"]:
        raise ValueError("archive SHA-256 mismatch")
    indices = manifest["archive"]["projection_indices"]
    partial = path.with_suffix(path.suffix + ".partial")
    rows, outside_month = 0, 0
    first_date, last_date = None, None
    with bz2.open(archive, "rt", newline="", encoding="utf-8") as source, partial.open(
            "x", newline="", encoding="utf-8") as output:
        reader = csv.reader(source, strict=True)
        if next(reader, None) != manifest["archive"]["columns"]:
            raise ValueError("unexpected archive CSV header")
        writer = csv.writer(output, lineterminator="\n")
        writer.writerow(manifest["columns"])
        for row in reader:
            if len(row) != len(manifest["archive"]["columns"]):
                raise ValueError(f"malformed archive row {rows + 1}")
            rows += 1
            values = [row[index] for index in indices]
            pickup = values[0]
            outside_month += not pickup.startswith("2015-01-")
            first_date = pickup if first_date is None else min(first_date, pickup)
            last_date = pickup if last_date is None else max(last_date, pickup)
            writer.writerow([f"csv:{rows}", *values])
            if rows % 100_000 == 0 and output.tell() > manifest["max_download_bytes"]:
                raise ValueError("projected corpus exceeds download byte limit")
    if rows != manifest["rows"]:
        raise ValueError(f"expected {manifest['rows']} archive rows, found {rows}")
    if candidate:
        manifest["archive"]["sha256"] = digest
        manifest["source_sha256"] = sha256(partial)
        manifest["observed_dates"] = {"minimum": first_date, "maximum": last_date,
                                      "outside_january_rows": outside_month}
        path.with_suffix(".candidate.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify_source(partial, manifest)
    publish_noreplace(partial, path)
    return manifest


def fetch(path, manifest, archive=None):
    if path.exists():
        return verify_source(path, manifest)
    archive = archive or path.parent / "aviz-original.csv.bz2"
    download_archive(archive, manifest)
    project_archive(archive, path, manifest)
    return manifest["rows"]


def import_source(path, manifest, connection):
    rows = verify_source(path, manifest)
    airport = MANIFEST.parent / manifest["airport"]["asset"]
    if sha256(airport) != manifest["airport"]["sha256"]:
        raise ValueError("pinned airport asset SHA-256 mismatch")
    try:
        import psycopg2
    except ImportError as error:
        raise RuntimeError("import requires Python psycopg2; fetch/verify need only the standard library") from error
    with psycopg2.connect(connection) as client:
        with client.cursor() as cursor:
            cursor.execute("SET pg_accel.enabled = off")
            cursor.execute("CREATE SCHEMA IF NOT EXISTS pgaccel_corpus")
            cursor.execute("SELECT to_regclass('pgaccel_corpus.nyc_yellow_2015_01')")
            if cursor.fetchone()[0] is not None:
                raise ValueError("corpus table already exists; refusing to overwrite it")
            cursor.execute("""
                CREATE TABLE pgaccel_corpus.nyc_yellow_2015_01 (
                    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                    source_id text NOT NULL UNIQUE,
                    pickup_datetime timestamp NOT NULL,
                    pickup_longitude float8, pickup_latitude float8,
                    vendor_id int4, passenger_count int4,
                    fare_amount numeric, trip_distance numeric, total_amount numeric,
                    coordinate_valid bool GENERATED ALWAYS AS
                      (coalesce(pickup_longitude BETWEEN -180 AND 180 AND
                                pickup_latitude BETWEEN -90 AND 90, false)) STORED,
                    pickup_geom geometry(Point,4326) GENERATED ALWAYS AS
                      (CASE WHEN pickup_longitude BETWEEN -180 AND 180 AND
                                 pickup_latitude BETWEEN -90 AND 90
                        THEN ST_SetSRID(ST_MakePoint(pickup_longitude,pickup_latitude),4326)
                       END) STORED,
                    cell h3index GENERATED ALWAYS AS
                      (CASE WHEN pickup_longitude BETWEEN -180 AND 180 AND
                                 pickup_latitude BETWEEN -90 AND 90
                        THEN public.h3_lat_lng_to_cell(point(pickup_latitude,pickup_longitude),15)
                       END) STORED
                )
            """)
            # The path can change after preverification, including while COPY
            # is running. Hash precisely the binary bytes consumed by COPY;
            # comparing a reopened path would miss replacement then restoration.
            with path.open("rb") as source:
                copied = CopyDigestReader(source)
                cursor.copy_expert("""
                    COPY pgaccel_corpus.nyc_yellow_2015_01
                    (source_id,pickup_datetime,pickup_longitude,pickup_latitude,
                     vendor_id,passenger_count,fare_amount,trip_distance,total_amount)
                    FROM STDIN WITH (FORMAT CSV, HEADER true)
                """, copied)
                if copied.digest.hexdigest() != manifest["source_sha256"]:
                    raise ValueError("COPY source SHA-256 mismatch; rolling back corpus import")
            cursor.execute("""
                SELECT count(*), count(*) FILTER (WHERE NOT coordinate_valid),
                       count(*) FILTER (WHERE pickup_longitude=0 AND pickup_latitude=0)
                FROM pgaccel_corpus.nyc_yellow_2015_01
            """)
            actual, invalid, zero = cursor.fetchone()
            if actual != rows:
                raise ValueError("imported row count mismatch")
            cursor.execute("""
                CREATE TABLE IF NOT EXISTS pgaccel_corpus.manifests (
                    dataset_id text PRIMARY KEY, source_sha256 text NOT NULL,
                    source_rows bigint NOT NULL, invalid_coordinate_rows bigint NOT NULL,
                    zero_coordinate_rows bigint NOT NULL, manifest jsonb NOT NULL,
                    imported_at timestamptz NOT NULL DEFAULT now()
                )
            """)
            cursor.execute("""
                INSERT INTO pgaccel_corpus.manifests
                (dataset_id,source_sha256,source_rows,invalid_coordinate_rows,zero_coordinate_rows,manifest)
                VALUES (%s,%s,%s,%s,%s,%s::jsonb)
            """, (manifest["dataset_id"], manifest["source_sha256"], rows, invalid, zero,
                  json.dumps(manifest, sort_keys=True)))
            cursor.execute("""
                CREATE OR REPLACE FUNCTION pgaccel_corpus.reject_fixture_mutation()
                RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
                  RAISE EXCEPTION 'Pinned corpus is immutable; import a new version instead';
                END $$;
                CREATE TRIGGER immutable_corpus BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE
                ON pgaccel_corpus.nyc_yellow_2015_01 FOR EACH STATEMENT
                EXECUTE FUNCTION pgaccel_corpus.reject_fixture_mutation();
                ANALYZE pgaccel_corpus.nyc_yellow_2015_01
            """)
    return {"rows": rows, "invalid_coordinates": invalid, "zero_coordinates": zero}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["fetch", "verify", "import", "build-candidate"])
    parser.add_argument("--source", type=Path, default=CACHE)
    parser.add_argument("--archive", type=Path, default=CACHE.parent / "aviz-original.csv.bz2")
    parser.add_argument("--connection", default="host=127.0.0.1 port=28818 dbname=postgres")
    args = parser.parse_args()
    manifest = read_manifest(candidate=args.command == "build-candidate")
    if args.command == "build-candidate":
        result = project_archive(args.archive, args.source, manifest, candidate=True)["source_sha256"]
    elif args.command == "fetch":
        result = fetch(args.source, manifest, args.archive)
    elif args.command == "verify":
        result = verify_source(args.source, manifest)
    else:
        result = import_source(args.source, manifest, args.connection)
    print(json.dumps({"dataset": manifest["dataset_id"], "result": result}, sort_keys=True))


if __name__ == "__main__":
    main()
