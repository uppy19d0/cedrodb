import json
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError


class PerformanceTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-performance-")
        self.path = Path(self.directory.name) / "database"
        self.db = Database(self.path)
        self.memory = self.db.collection("memory")
        self.memory.put_many({str(i): {
            "tenant": str(i % 10), "text": "local memory", "embedding": [1, 0],
        } for i in range(100)})

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_diagnostics_inventory_and_reopen_reset(self):
        self.memory.create_index("tenant")
        report = self.db.diagnostics()
        self.assertEqual(report["stats"]["records"], 100)
        self.assertEqual(report["collections"][0]["index_fields"], ["tenant"])
        self.assertEqual(report["session"]["commit"]["successes"], 1)
        self.assertEqual(report["limits"]["max_state_bytes"], 67_108_864)
        self.db.close()
        self.db = Database(self.path)
        restored = self.db.diagnostics()
        self.assertEqual(restored["stats"]["records"], 100)
        self.assertEqual(restored["session"]["commit"]["attempts"], 0)
        self.assertEqual(restored["collections"][0]["index_fields"], [])

    def test_query_and_search_profiles_preserve_results_without_writes(self):
        filters = [{"field": "tenant", "op": "eq", "value": "0"}]
        before = self.db.stats()
        scanned = self.memory.profile_query(filters, limit=1000)
        searched = self.memory.profile_search(text="local", vector=[1, 0], filters=filters)
        self.memory.create_index("tenant")
        indexed = self.memory.profile_query(filters, limit=1000)
        retrieval = self.memory.profile_search(text="local", vector=[1, 0], filters=filters)
        self.assertEqual(scanned["page"]["records"], indexed["page"]["records"])
        self.assertEqual(searched["page"]["hits"], retrieval["page"]["hits"])
        self.assertEqual(scanned["plan"]["scanned"], 100)
        self.assertEqual(indexed["plan"]["scanned"], 10)
        self.assertEqual(retrieval["plan"]["used_index"], "tenant")
        self.assertGreaterEqual(retrieval["elapsed_micros"], 0)
        after = self.db.stats()
        self.assertEqual((before["latest_cursor"], before["log_bytes"]),
                         (after["latest_cursor"], after["log_bytes"]))
        session = self.db.diagnostics()["session"]
        self.assertEqual(session["query"]["successes"], 2)
        self.assertEqual(session["search"]["successes"], 2)

    def test_profile_validation_and_closed_handle(self):
        with self.assertRaises(CedroError):
            self.memory.profile_search(vector=[1, 0], max_scanned=1)
        with self.assertRaises(TypeError):
            self.memory.profile_search(vector=[True, 0])
        with self.assertRaises(CedroError):
            self.memory.profile_query(limit=1001)
        report = self.db.diagnostics()
        self.assertEqual(report["session"]["search"]["failures"], 1)
        self.assertEqual(report["session"]["query"]["failures"], 1)
        self.db.close()
        for call in (self.db.diagnostics, self.memory.profile_query,
                     lambda: self.memory.profile_search(text="local")):
            with self.assertRaises(DatabaseClosedError):
                call()

    def test_cli_isolated_benchmark_and_doctor(self):
        result = subprocess.run([
            sys.executable, "-m", "cedrodb", "benchmark", "--records", "20",
            "--iterations", "2", "--warmup", "0", "--batch-size", "7",
        ], check=True, capture_output=True, text=True)
        report = json.loads(result.stdout)
        self.assertTrue(all(report["correctness"].values()))
        self.assertEqual(report["diagnostics"]["stats"]["records"], 20)
        self.assertEqual(report["measurements"]["query_scan"]["plan"]["scanned"], 20)
        self.assertEqual(report["measurements"]["query_indexed"]["plan"]["scanned"], 2)
        doctor = subprocess.run([sys.executable, "-m", "cedrodb", "doctor"],
                                check=True, capture_output=True, text=True)
        self.assertTrue(json.loads(doctor.stdout)["native_extension_loaded"])
        invalid = subprocess.run([sys.executable, "-m", "cedrodb", "benchmark", "--records", "0"],
                                 capture_output=True, text=True)
        self.assertEqual(invalid.returncode, 2)
        self.assertEqual(self.db.stats()["records"], 100)
