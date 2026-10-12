from contextlib import contextmanager
import bz2
import csv
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from scripts import nyc_taxi_corpus as corpus


class NycCorpusTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = self.root / "source.csv"
        self.source.write_text('source_id,lon,lat\na,-73.9,40.7\nb,0,0\nc,,\n')
        self.manifest = {
            "schema_version": 1,
            "source_sha256": hashlib.sha256(self.source.read_bytes()).hexdigest(),
            "columns": ["source_id", "lon", "lat"],
            "rows": 3,
            "max_download_bytes": 1000,
        }

    def test_pinned_source_keeps_null_and_zero_coordinates(self):
        self.assertEqual(corpus.verify_source(self.source, self.manifest), 3)

    def test_modified_source_is_rejected(self):
        self.source.write_text(self.source.read_text().replace("40.7", "41.7"))
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            corpus.verify_source(self.source, self.manifest)

    def test_wrong_header_row_count_and_size_fail_closed(self):
        for changes, message in [({"columns": ["lat", "lon", "source_id"]}, "header"),
                                 ({"rows": 4}, "source rows"),
                                 ({"max_download_bytes": 2}, "byte limit")]:
            with self.subTest(changes=changes):
                with self.assertRaisesRegex(ValueError, message):
                    corpus.verify_source(self.source, self.manifest | changes)

    def test_unpinned_manifest_fails_before_network(self):
        manifest = self.root / "manifest.json"
        manifest.write_text(json.dumps(self.manifest | {"source_sha256": "UNPINNED"}))
        with self.assertRaisesRegex(ValueError, "reviewed pinned"):
            corpus.read_manifest(manifest)
        manifest.write_text(json.dumps(self.manifest | {"archive": {"sha256": "UNPINNED"}}))
        with self.assertRaisesRegex(ValueError, "archive must have a reviewed pinned"):
            corpus.read_manifest(manifest)

    def test_existing_verified_download_needs_no_network(self):
        with patch("urllib.request.urlopen", side_effect=AssertionError("unexpected network")):
            self.assertEqual(corpus.fetch(self.source, self.manifest), 3)

    def test_fetch_rejects_oversize_and_never_publishes_partial(self):
        target = self.root / "download.csv"
        manifest = self.manifest | {"archive": {"url": "https://example.invalid/data.bz2",
                                               "max_download_bytes": 1}}
        with self.source.open("rb") as stream, patch("urllib.request.urlopen", return_value=stream):
            with self.assertRaisesRegex(ValueError, "byte limit"):
                corpus.download_archive(target, manifest)
        self.assertFalse(target.exists())
        self.assertTrue(target.with_suffix(".csv.partial").exists())

    def test_download_never_replaces_concurrently_created_destination(self):
        target = self.root / "download.bz2"
        manifest = self.manifest | {"archive": {
            "url": "https://example.invalid/data.bz2", "max_download_bytes": 1000,
            "sha256": corpus.sha256(self.source),
        }}

        def open_download(*args, **kwargs):
            target.write_bytes(b"another writer's archive")
            return self.source.open("rb")

        with patch("urllib.request.urlopen", side_effect=open_download):
            with self.assertRaises(FileExistsError):
                corpus.download_archive(target, manifest)
        self.assertEqual(target.read_bytes(), b"another writer's archive")
        self.assertEqual(target.with_suffix(".bz2.partial").read_bytes(), self.source.read_bytes())

    def test_interleaved_download_cannot_write_another_producers_partial(self):
        target = self.root / "shared.bz2"
        partial = target.with_suffix(".bz2.partial")
        manifest = self.manifest | {"archive": {
            "url": "https://example.invalid/data.bz2", "max_download_bytes": 1000,
            "sha256": corpus.sha256(self.source),
        }}

        def open_first_download(*args, **kwargs):
            # The first producer has exclusively created its partial before
            # opening the network stream. A second producer must not open it.
            self.assertEqual(partial.read_bytes(), b"")
            with self.assertRaises(FileExistsError) as failure:
                corpus.download_archive(target, manifest)
            self.assertEqual(failure.exception.filename, str(partial))
            self.assertEqual(partial.read_bytes(), b"")
            return self.source.open("rb")

        with patch("urllib.request.urlopen", side_effect=open_first_download) as download:
            corpus.download_archive(target, manifest)
        self.assertEqual(download.call_count, 1)
        self.assertEqual(target.read_bytes(), self.source.read_bytes())
        self.assertFalse(partial.exists())

    def test_verify_rejects_malformed_csv_quotes_even_with_matching_pin(self):
        for last_field in ['"40.7', '"40.7"trailing\n']:
            with self.subTest(last_field=last_field):
                self.source.write_text('source_id,lon,lat\na,-73.9,' + last_field)
                manifest = self.manifest | {"source_sha256": corpus.sha256(self.source), "rows": 1}
                with self.assertRaises(csv.Error):
                    corpus.verify_source(self.source, manifest)

    def test_airport_asset_preserves_pinned_complete_geometry(self):
        manifest = json.loads(corpus.MANIFEST.read_text())
        airport = corpus.MANIFEST.parent / manifest["airport"]["asset"]
        self.assertEqual(corpus.sha256(airport), manifest["airport"]["sha256"])
        self.assertTrue(airport.read_text().startswith("SRID=4326;MULTIPOLYGON"))
        self.assertEqual(manifest["airport"]["components"], 4)

    def archive_fixture(self):
        body = ("date,lon,lat\n"
                "2015-01-01,0,0\n"
                "2014-12-31,-73.123456789123456,40.7\n"
                "2015-01-01,0,0\n"
                "2015-01-02,,\n")
        archive = self.root / "archive.bz2"
        archive.write_bytes(bz2.compress(body.encode()))
        manifest = self.manifest | {
            "columns": ["source_id", "date", "lon", "lat"], "rows": 4,
            "archive": {"columns": ["date", "lon", "lat"], "projection_indices": [0, 1, 2],
                        "sha256": corpus.sha256(archive), "max_download_bytes": 1000},
        }
        return archive, manifest

    def test_projection_keeps_order_duplicates_outliers_nulls_and_decimal_text(self):
        archive, manifest = self.archive_fixture()
        target = self.root / "projection.csv"
        candidate = corpus.project_archive(archive, target, manifest, candidate=True)
        self.assertEqual(target.read_text(), "source_id,date,lon,lat\n"
                         "csv:1,2015-01-01,0,0\n"
                         "csv:2,2014-12-31,-73.123456789123456,40.7\n"
                         "csv:3,2015-01-01,0,0\n"
                         "csv:4,2015-01-02,,\n")
        self.assertEqual(candidate["observed_dates"]["outside_january_rows"], 1)
        self.assertEqual(corpus.verify_source(target, candidate), 4)
        second = self.root / "reproduced.csv"
        corpus.project_archive(archive, second, candidate)
        self.assertEqual(second.read_bytes(), target.read_bytes())

    def test_projection_rejects_wrong_hash_header_and_count(self):
        archive, manifest = self.archive_fixture()
        for changes, error in [
            ({"archive": manifest["archive"] | {"sha256": "0" * 64}}, "SHA-256"),
            ({"archive": manifest["archive"] | {"columns": ["wrong"]}}, "header"),
            ({"rows": 5}, "archive rows"),
        ]:
            with self.subTest(error=error):
                target = self.root / (error + ".csv")
                with self.assertRaisesRegex(ValueError, error):
                    corpus.project_archive(archive, target, manifest | changes)
                self.assertFalse(target.exists())

    def test_truncated_bzip_is_never_published(self):
        archive, manifest = self.archive_fixture()
        archive.write_bytes(archive.read_bytes()[:-10])
        target = self.root / "truncated.csv"
        with self.assertRaises(EOFError):
            corpus.project_archive(archive, target, manifest, candidate=True)
        self.assertFalse(target.exists())

    def test_projection_never_replaces_concurrently_created_destination(self):
        archive, manifest = self.archive_fixture()
        reference = self.root / "reference.csv"
        manifest = corpus.project_archive(archive, reference, manifest, candidate=True)
        target = self.root / "competing.csv"
        verify = corpus.verify_source

        def verify_then_compete(source, expected):
            rows = verify(source, expected)
            target.write_bytes(b"another writer's corpus")
            return rows

        with patch.object(corpus, "verify_source", side_effect=verify_then_compete):
            with self.assertRaises(FileExistsError):
                corpus.project_archive(archive, target, manifest)
        self.assertEqual(target.read_bytes(), b"another writer's corpus")
        self.assertEqual(target.with_suffix(".csv.partial").read_bytes(), reference.read_bytes())

    def test_interleaved_projection_cannot_write_verified_partial(self):
        archive, manifest = self.archive_fixture()
        reference = self.root / "reference.csv"
        manifest = corpus.project_archive(archive, reference, manifest, candidate=True)
        competitor = self.root / "competing.bz2"
        competitor.write_bytes(bz2.compress(bz2.decompress(archive.read_bytes()).replace(
            b"40.7", b"80.7")))
        target = self.root / "shared.csv"
        partial = target.with_suffix(".csv.partial")
        verify = corpus.verify_source

        def verify_then_start_competitor(source, expected):
            rows = verify(source, expected)
            with self.assertRaises(FileExistsError) as failure:
                corpus.project_archive(competitor, target, manifest, candidate=True)
            self.assertEqual(failure.exception.filename, str(partial))
            self.assertEqual(partial.read_bytes(), reference.read_bytes())
            return rows

        with patch.object(corpus, "verify_source", side_effect=verify_then_start_competitor):
            corpus.project_archive(archive, target, manifest)
        self.assertEqual(target.read_bytes(), reference.read_bytes())
        self.assertFalse(partial.exists())

    def test_projection_rejects_malformed_csv_quotes_before_publication(self):
        for index, last_field in enumerate(['"40.7', '"40.7"trailing\n']):
            with self.subTest(last_field=last_field):
                archive, manifest = self.archive_fixture()
                archive.write_bytes(bz2.compress(('date,lon,lat\n2015-01-01,0,' + last_field).encode()))
                manifest |= {"rows": 1}
                target = self.root / f"malformed-{index}.csv"
                with self.assertRaises(csv.Error):
                    corpus.project_archive(archive, target, manifest, candidate=True)
                self.assertFalse(target.exists())
                self.assertFalse(target.with_suffix(".candidate.json").exists())

    def test_import_hashes_copy_bytes_and_rolls_back_replaced_source(self):
        manifest = corpus.read_manifest()
        original = (",".join(manifest["columns"]) + "\r\n"
                    "original,2015-01-01,-73.9,40.7,1,2,12.34,1.25,15.34\r\n").encode()
        replacement = original.replace(b"12.34", b"92.34")
        manifest |= {"rows": 1, "source_sha256": hashlib.sha256(original).hexdigest()}
        for changed, restore_after_copy in [(False, False), (True, False), (True, True)]:
            with self.subTest(changed=changed, restore_after_copy=restore_after_copy):
                self.source.write_bytes(original)
                state = {"published": [], "staged": [], "copied": b"", "rollback": False}

                class Cursor:
                    def __enter__(self):
                        return self

                    def __exit__(self, *args):
                        pass

                    def execute(self, sql, params=None):
                        self.sql = sql
                        if "CREATE TABLE" in sql:
                            state["staged"].append(sql)

                    def fetchone(self):
                        return (None,) if "to_regclass" in self.sql else (1, 0, 0)

                    def copy_expert(self, sql, source):
                        chunks = []
                        while block := source.read(7):
                            chunks.append(block.encode() if isinstance(block, str) else block)
                        state["copied"] = b"".join(chunks)
                        if restore_after_copy:
                            self_source.write_bytes(original)

                @contextmanager
                def connect(_):
                    if changed:
                        self.source.write_bytes(replacement)
                    try:
                        yield SimpleNamespace(cursor=Cursor)
                    except Exception:
                        state["rollback"] = True
                        state["staged"].clear()
                        raise
                    else:
                        state["published"] = state["staged"].copy()

                self_source = self.source
                with patch.dict(sys.modules, {"psycopg2": SimpleNamespace(connect=connect)}):
                    if changed:
                        with self.assertRaisesRegex(ValueError, "COPY.*SHA-256 mismatch"):
                            corpus.import_source(self.source, manifest, "test")
                        self.assertTrue(state["rollback"])
                        self.assertEqual(state["published"], [])
                        self.assertEqual(state["staged"], [])
                        self.assertEqual(state["copied"], replacement)
                    else:
                        self.assertEqual(corpus.import_source(self.source, manifest, "test"),
                                         {"rows": 1, "invalid_coordinates": 0, "zero_coordinates": 0})
                        self.assertEqual(len(state["published"]), 2)
                        self.assertEqual(state["copied"], original)

    @unittest.skipUnless(os.environ.get("PG_ACCEL_CORPUS_TEST_DSN"), "opt-in live importer test")
    def test_live_import_preserves_rows_generated_columns_and_immutability(self):
        import psycopg2
        manifest = json.loads(corpus.MANIFEST.read_text())
        self.source.write_text(
            ",".join(manifest["columns"]) + "\n"
            "valid,2015-01-01,-73.9,40.7,1,2,12.34,1.25,15.34\n"
            "zero,2015-01-01,0,0,2,1,0,0,0\n"
            "missing,2015-01-01,,,1,1,1,1,1\n"
            "outside,2015-01-01,181,91,1,1,1,1,1\n")
        manifest |= {"source_sha256": corpus.sha256(self.source), "rows": 4}
        client = psycopg2.connect(os.environ["PG_ACCEL_CORPUS_TEST_DSN"])
        self.addCleanup(client.close)

        @contextmanager
        def rollback_connection(_):
            try:
                yield client
                with client.cursor() as cursor:
                    cursor.execute("SELECT source_id,coordinate_valid,pickup_geom IS NULL,cell IS NULL "
                                   "FROM pgaccel_corpus.nyc_yellow_2015_01 ORDER BY id")
                    self.assertEqual(cursor.fetchall(), [("valid", True, False, False),
                        ("zero", True, False, False), ("missing", False, True, True),
                        ("outside", False, True, True)])
                    cursor.execute("SAVEPOINT mutation")
                    with self.assertRaisesRegex(psycopg2.Error, "immutable"):
                        cursor.execute("TRUNCATE pgaccel_corpus.nyc_yellow_2015_01")
                    cursor.execute("ROLLBACK TO SAVEPOINT mutation")
            finally:
                client.rollback()

        with patch("psycopg2.connect", rollback_connection):
            self.assertEqual(corpus.import_source(self.source, manifest, "test"),
                             {"rows": 4, "invalid_coordinates": 2, "zero_coordinates": 1})


if __name__ == "__main__":
    unittest.main()
