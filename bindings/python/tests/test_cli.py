import json
from importlib.metadata import distribution
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database


class CliTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-cli-")
        self.root = Path(self.directory.name)
        self.db = self.root / "database"

    def tearDown(self):
        self.directory.cleanup()

    def run_cli(self, *args, input=None, status=0, env=None):
        environment = os.environ.copy()
        environment.pop("CEDRODB_PATH", None)
        if env:
            environment.update(env)
        result = subprocess.run([sys.executable, "-m", "cedrodb", *map(str, args)],
                                input=input, capture_output=True, text=True, env=environment)
        self.assertEqual(result.returncode, status, result.stderr)
        return result

    def data(self, *args, **kwargs):
        return json.loads(self.run_cli(*args, **kwargs).stdout)

    def initialize(self):
        return self.data("init", "--db", self.db)

    def test_package_registers_shared_console_entry(self):
        from cedrodb.cli import main
        entry = next(entry for entry in distribution("cedrodb").entry_points
                     if entry.group == "console_scripts" and entry.name == "cedrodb")
        self.assertIs(entry.load(), main)

    def test_help_version_and_unknown_options(self):
        help = self.run_cli("--help").stdout
        self.assertIn("profile-search", help)
        self.assertIn("restore", help)
        self.assertIn("--stdin", self.run_cli("put", "--help").stdout)
        self.assertIn("cedrodb ", self.run_cli("--version").stdout)
        result = self.run_cli("--json", "unknown-command", status=2)
        self.assertEqual(json.loads(result.stderr)["error"]["code"], "invalid_arguments")
        self.assertFalse(self.db.exists())

    def test_init_requires_new_database_and_existing_commands_do_not_create(self):
        self.run_cli("stats", "--db", self.db, status=2)
        self.assertFalse(self.db.exists())
        self.assertTrue(self.initialize()["created"])
        self.run_cli("init", "--db", self.db, status=2)
        self.assertEqual(self.data("stats", "--db", self.db)["records"], 0)
        self.run_cli("get", "items", "x", "--db", self.db, "--unknown", status=2)

    def test_put_get_null_and_missing_are_distinct(self):
        self.initialize()
        self.data("put", "items", "null", "--db", self.db, "--value", "null")
        stored = self.data("get", "items", "null", "--db", self.db, "--json")
        self.assertTrue(stored["found"])
        self.assertIsNone(stored["value"])
        missing = self.data("get", "items", "missing", "--db", self.db, status=3)
        self.assertEqual(missing, {"found": False, "key": "missing"})

    def test_stdin_file_input_unicode_and_environment_path(self):
        self.initialize()
        document = {"text": "Decisión de caché 日本語", "number": 1.23456789012345}
        self.data("--json", "put", "items", "stdin", "--db", self.db, "--stdin", input=json.dumps(document))
        source = self.root / "document.json"
        source.write_text(json.dumps(document), encoding="utf-8")
        self.data("put", "items", "file", "--db", self.db, "--file", source)
        self.assertEqual(self.data("get", "items", "stdin", env={"CEDRODB_PATH": str(self.db)})["value"], document)
        self.assertEqual(self.data("get", "items", "file", "--db", self.db)["value"], document)

    def test_json_input_rejects_duplicates_nonfinite_and_conflicting_sources(self):
        self.initialize()
        before = (self.db / "cedro.log").read_bytes()
        for value in ['{"x":1,"x":2}', '{"x":NaN}', '{"x":1e9999}', '{']:
            result = self.run_cli("put", "items", "bad", "--db", self.db, "--stdin", "--json", input=value, status=2)
            self.assertFalse(result.stdout)
            self.assertIn("error", json.loads(result.stderr))
        self.run_cli("put", "items", "bad", "--db", self.db, "--stdin", "--value", "{}", status=2)
        self.assertEqual((self.db / "cedro.log").read_bytes(), before)

    def test_atomic_batches_and_keyset_pages(self):
        self.initialize()
        operations = [{"kind": "put", "collection": "items", "key": key, "value": {"tenant": "demo", "number": index}} for index, key in enumerate(["a", "b", "c"])]
        self.assertEqual(self.data("batch", "--db", self.db, "--stdin", input=json.dumps(operations))["operations"], 3)
        page = self.data("find", "items", "--db", self.db, "--limit", "1", "--filter", '{"field":"tenant","op":"eq","value":"demo"}', "--index", "tenant")
        self.assertEqual(page["records"][0]["key"], "a")
        self.assertEqual(page["used_index"], "tenant")
        next_page = self.data("query", "items", "--db", self.db, "--after", page["next_after"])
        self.assertEqual([record["key"] for record in next_page["records"]], ["b", "c"])
        before = (self.db / "cedro.log").read_bytes()
        bad = [{"kind": "put", "collection": "items", "key": "safe", "value": 1}, {"kind": "unknown"}]
        failed = subprocess.run([sys.executable, "-m", "cedrodb", "batch", "--db", str(self.db), "--stdin"], input=json.dumps(bad), text=True, capture_output=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual((self.db / "cedro.log").read_bytes(), before)

    def test_text_vector_hybrid_search_and_profiles_use_real_engine(self):
        self.initialize()
        # These synthetic vectors test contracts, not embedding quality.
        for key, value in [("refund", {"text": "refund policy", "model": "test", "embedding": [1, 0]}), ("deploy", {"text": "deployment checklist", "model": "test", "embedding": [0, 1]})]:
            self.data("put", "memory", key, "--db", self.db, "--stdin", input=json.dumps(value))
        common = ["memory", "--db", self.db, "--filter", '{"field":"model","op":"eq","value":"test"}', "--index", "model"]
        self.assertEqual(self.data("search", *common, "--text", "refund")["mode"], "text")
        vector = self.data("search", *common, "--vector", "[1,0]")
        self.assertEqual(vector["mode"], "vector")
        self.assertEqual(vector["hits"][0]["key"], "refund")
        profile = self.data("profile-search", *common, "--text", "refund", "--vector", "[1,0]")
        self.assertEqual(profile["page"]["mode"], "hybrid")
        self.assertEqual(profile["plan"]["used_index"], "model")
        self.assertGreaterEqual(profile["elapsed_micros"], 0)
        query = self.data("profile-query", "memory", "--db", self.db, "--limit", "1")
        self.assertEqual(query["plan"]["returned"], 1)
        self.run_cli("search", *common, "--text", "refund", "--max-scanned", "1", status=1)
        self.assertEqual(self.data("stats", "--db", self.db)["records"], 2)

    def test_backups_restore_inventory_changes_and_deletion(self):
        self.initialize()
        self.data("put", "items", "one", "--db", self.db, "--value", '{"count":1}')
        self.assertEqual(self.data("collections", "--db", self.db)["collections"][0]["collection"], "items")
        self.assertEqual(self.data("verify", "--db", self.db)["records"], 1)
        self.assertIn("limits", self.data("diagnostics", "--db", self.db))
        self.assertEqual(len(self.data("changes", "--db", self.db)["transactions"]), 1)
        backup = self.root / "snapshot.cedrobak"
        self.data("backup", backup, "--db", self.db)
        self.run_cli("backup", backup, "--db", self.db, status=1)
        restored = self.root / "restored"
        self.data("restore", backup, "--db", restored)
        self.run_cli("restore", backup, "--db", restored, status=1)
        self.assertEqual(self.data("get", "items", "one", "--db", restored)["value"], {"count": 1})
        self.assertTrue(self.data("delete", "items", "one", "--db", self.db)["deleted"])
        self.assertFalse(self.data("delete", "items", "one", "--db", self.db)["deleted"])

    def test_exclusive_lock_error_has_json_and_next_process_can_reopen(self):
        self.initialize()
        with Database(self.db):
            result = self.run_cli("stats", "--db", self.db, "--json", status=1)
            self.assertEqual(json.loads(result.stderr)["error"]["code"], "operation_failed")
            self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(self.data("stats", "--db", self.db)["records"], 0)
