#!/usr/bin/env python3
"""Load GeoJSON boundary data from plazafyi/boundaries into PostgreSQL.

Requires: psycopg2, PostGIS extension.

Usage:
    python3 load_boundaries.py --connection "host=localhost port=28817 dbname=pg_accel_test user=postgres"
    python3 load_boundaries.py --boundary-dir /tmp/boundaries

The script creates a `real_boundaries` table and bulk-loads census + neighborhood
GeoJSON files.  It is idempotent — re-running truncates and reloads.
"""

import argparse
import json
import os
import sys
import time

try:
    import psycopg2
except ImportError:
    print("ERROR: psycopg2 required.  pip install psycopg2-binary", file=sys.stderr)
    sys.exit(1)


DDL = """
DROP TABLE IF EXISTS real_boundaries CASCADE;
CREATE TABLE real_boundaries (
    id serial PRIMARY KEY,
    name text NOT NULL,
    fullname text NOT NULL DEFAULT '',
    state text NOT NULL DEFAULT '',
    boundary_type text NOT NULL,
    population int,
    aland bigint,
    awater bigint,
    vertex_count int NOT NULL DEFAULT 0,
    num_parts int NOT NULL DEFAULT 0,
    geom geometry(MultiPolygon, 4326) NOT NULL
);
"""

INDEXES = """
CREATE INDEX ON real_boundaries USING gist (geom);
CREATE INDEX ON real_boundaries (boundary_type);
CREATE INDEX ON real_boundaries (vertex_count);
CREATE INDEX ON real_boundaries (state);
ANALYZE real_boundaries;
"""


def count_vertices(coords):
    """Count total vertices in a MultiPolygon coordinate array."""
    total = 0
    for polygon in coords:
        for ring in polygon:
            total += len(ring)
    return total


def count_parts(coords):
    """Count polygon parts in a MultiPolygon."""
    return len(coords)


def parse_geojson(filepath, boundary_type):
    """Parse a single GeoJSON file into a row dict."""
    with open(filepath, "r") as f:
        data = json.load(f)

    props = data.get("properties", {})
    coords = data.get("coordinates", [])

    name = props.get("name", "") or props.get("fullname", "") or ""
    fullname = props.get("fullname", "") or name
    state = props.get("stusps", "") or props.get("state_name", "") or props.get("state", "") or ""
    population = props.get("population")
    aland = props.get("aland")
    awater = props.get("awater")

    if population is not None:
        try:
            population = int(population)
        except (ValueError, TypeError):
            population = None
    if aland is not None:
        try:
            aland = int(aland)
        except (ValueError, TypeError):
            aland = None
    if awater is not None:
        try:
            awater = int(awater)
        except (ValueError, TypeError):
            awater = None

    geojson_str = json.dumps({"type": "MultiPolygon", "coordinates": coords})

    return {
        "name": name,
        "fullname": fullname,
        "state": state,
        "boundary_type": boundary_type,
        "population": population,
        "aland": aland,
        "awater": awater,
        "vertex_count": count_vertices(coords),
        "num_parts": count_parts(coords),
        "geojson": geojson_str,
    }


def load_directory(cur, dirpath, boundary_type, batch_size=500):
    """Load all GeoJSON files from a directory."""
    files = sorted(f for f in os.listdir(dirpath) if f.endswith(".geojson"))
    total = len(files)
    loaded = 0
    skipped = 0

    batch = []
    for i, fname in enumerate(files):
        try:
            row = parse_geojson(os.path.join(dirpath, fname), boundary_type)
            batch.append(row)
        except Exception as e:
            skipped += 1
            if skipped <= 5:
                print(f"  WARN: skipping {fname}: {e}", file=sys.stderr)
            continue

        if len(batch) >= batch_size:
            insert_batch(cur, batch)
            loaded += len(batch)
            batch = []
            if (loaded % 5000) == 0:
                print(f"  {boundary_type}: {loaded}/{total} loaded...", file=sys.stderr)

    if batch:
        insert_batch(cur, batch)
        loaded += len(batch)

    print(f"  {boundary_type}: {loaded} loaded, {skipped} skipped (of {total})", file=sys.stderr)
    return loaded


def insert_batch(cur, batch):
    """Insert a batch of rows."""
    sql = """
    INSERT INTO real_boundaries
        (name, fullname, state, boundary_type, population, aland, awater,
         vertex_count, num_parts, geom)
    VALUES
        (%s, %s, %s, %s, %s, %s, %s, %s, %s, ST_SetSRID(ST_GeomFromGeoJSON(%s), 4326))
    """
    values = [
        (
            r["name"], r["fullname"], r["state"], r["boundary_type"],
            r["population"], r["aland"], r["awater"],
            r["vertex_count"], r["num_parts"], r["geojson"],
        )
        for r in batch
    ]
    cur.executemany(sql, values)


def main():
    parser = argparse.ArgumentParser(description="Load boundary GeoJSON into PostgreSQL")
    parser.add_argument(
        "--connection", "-c",
        default="host=localhost port=28817 dbname=pg_accel_test user=postgres",
        help="PostgreSQL connection string",
    )
    parser.add_argument(
        "--boundary-dir", "-d",
        default="/tmp/boundaries",
        help="Path to cloned plazafyi/boundaries repo",
    )
    args = parser.parse_args()

    census_dir = os.path.join(args.boundary_dir, "census")
    neighborhoods_dir = os.path.join(args.boundary_dir, "neighborhoods")

    if not os.path.isdir(census_dir):
        print(f"ERROR: {census_dir} not found. Clone the repo first:", file=sys.stderr)
        print("  git clone --depth 1 https://github.com/plazafyi/boundaries /tmp/boundaries", file=sys.stderr)
        sys.exit(1)

    conn = psycopg2.connect(args.connection)
    conn.autocommit = False
    cur = conn.cursor()

    print("Creating real_boundaries table...", file=sys.stderr)
    cur.execute(DDL)
    conn.commit()

    t0 = time.time()
    total = 0

    print("Loading census boundaries...", file=sys.stderr)
    total += load_directory(cur, census_dir, "census")
    conn.commit()

    if os.path.isdir(neighborhoods_dir):
        print("Loading neighborhood boundaries...", file=sys.stderr)
        total += load_directory(cur, neighborhoods_dir, "neighborhood")
        conn.commit()

    print("Creating indexes...", file=sys.stderr)
    cur.execute(INDEXES)
    conn.commit()

    elapsed = time.time() - t0
    print(f"\nDone: {total} boundaries loaded in {elapsed:.1f}s", file=sys.stderr)

    # Print summary stats
    cur.execute("""
        SELECT boundary_type, count(*),
               avg(vertex_count)::int, max(vertex_count),
               avg(num_parts)::int, max(num_parts)
        FROM real_boundaries GROUP BY boundary_type ORDER BY 1
    """)
    print("\n  Type          | Count  | Avg Verts | Max Verts | Avg Parts | Max Parts")
    print("  " + "-" * 72)
    for row in cur.fetchall():
        print(f"  {row[0]:<14} | {row[1]:>6} | {row[2]:>9} | {row[3]:>9} | {row[4]:>9} | {row[5]:>9}")

    cur.close()
    conn.close()


if __name__ == "__main__":
    main()
