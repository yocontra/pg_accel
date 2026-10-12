# NYC yellow taxis: January 2015 original-coordinate corpus

This opt-in corpus measures real H3 grouping, airport membership and monetary
rollups. It is a January-file characterization, not the full-year NYC corpus or
a release performance qualification. Unsupported queries remain in the report.

## Source and preservation

The source is the original-format monthly CSV archived by
[Inria Aviz](https://www.aviz.fr/nyc-taxi/), not the similarly named cleaned file.
The original publisher is the [NYC Taxi and Limousine Commission](https://www.nyc.gov/site/tlc/about/tlc-trip-record-data.page).
Modern TLC Parquet files use taxi-zone IDs rather than these raw coordinates.
The archive and derived projection have separate SHA-256 pins in `manifest.json`;
mirror bytes are not claimed identical to the current NYC Open Data export.

The importer preserves every archive row, source order, decimal text, duplicate
records, missing/invalid coordinates, valid `(0,0)` points, and outliers outside
NYC or the nominal month. `source_id` is the original CSV row ordinal, not a trip
identifier. No centroids, synthetic replication, rounding or deduplication are
used. Geometry and resolution-15 H3 cells are prepared only for valid WGS84
coordinate ranges; preprocessing is outside query timing. The imported table
rejects mutation and import never overwrites it.

The JFK boundary comes from the pinned 2026 TLC taxi-zone archive (LocationID
132), transformed by GDAL 3.13.1 from EPSG:2263 to EPSG:4326. All four polygon
components and 2,237 coordinates are retained. It is deliberately not described
as the historical 2015 boundary. `manifest.json` records both source and derived
geometry hashes.

## Prepare and run

Allow room for the compressed archive, projected CSV, database copy, indexes
and benchmark working tables (roughly 10 GiB of free space). Fetch/verification
use Python's standard library; import additionally needs `psycopg2`. The target
database must have `pg_accel`, `postgis` and `h3` installed.

```sh
python3 scripts/nyc_taxi_corpus.py fetch
python3 scripts/nyc_taxi_corpus.py verify
python3 scripts/nyc_taxi_corpus.py import \
  --connection 'host=127.0.0.1 port=28818 dbname=postgres'
cargo build -p pg_accel_bench --release --locked
target/release/pg_accel_bench run --category nyc_taxi \
  --connection 'host=127.0.0.1 port=28818 dbname=postgres' \
  --iterations 10 --warmup 5 --cache-mode warm --timing raw \
  --capture-plans --artifacts-dir benchmarks/artifacts/nyc-taxi
```

The default benchmark suite does not download, import or run this corpus.
Explicit selection runs a 1M-row diagnostic prefix and the complete source
file, with these seven queries:

| Workload | SQL behavior |
| --- | --- |
| `nyc_taxi_h3_res9` | H3 parent grouping, including the NULL group |
| `nyc_taxi_airport_indexed` | Exact JFK intersection with a GiST index available |
| `nyc_taxi_airport_scan` | Same complete boundary without a spatial index |
| `nyc_taxi_fare_rollup` | Vendor/passenger grouping with exact NUMERIC SUM/AVG |
| `nyc_taxi_mixed_rollup` | Vendor grouping with integer passenger SUM/AVG and exact NUMERIC fare/distance aggregates |
| `nyc_taxi_filtered_rollup` | Exact fare/distance aggregation restricted to vendor 1 by query-level WHERE |
| `nyc_taxi_airport_rollup` | Exact vendor fare/distance/passenger rollup for pickups inside the complete JFK boundary, without GiST |

Every measured cell retains native/accelerated exact result comparison. H3
also has an independent parent-bit oracle. Plans, dispatch/decline evidence,
resident load and first-use/warm measurements use the existing harness. The
source manifest is copied into each artifact directory, including failed setup
attempts. Neither individual queries nor this small corpus authorize a
whole-suite headline speedup; the same restriction applies to resumed runs.

## Pin maintenance

An upstream checksum change is a failure, not permission to silently repin.
For an intentionally reviewed new source, download the archive separately and
run `build-candidate` with a new `--source` path. This produces a proposed
`.candidate.json`; it does not modify the repository manifest or import data.
Review provenance, counts, observed dates and hashes before updating the pin.
Failed `.partial` files remain diagnostic evidence and are never imported.

Hermetic tests: `python3 -m unittest scripts.tests.test_nyc_taxi_corpus`.
Set `PG_ACCEL_CORPUS_TEST_DSN` to run the additional transaction-rolled-back
import test against a database without an existing corpus table.
