from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError, MAX_SAFE_INTEGER


class AgentTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-agents-")
        self.path = Path(self.directory.name) / "agent.cedro"
        self.db = Database(self.path)
        self.agent = self.db.agent("assistant")

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_checkpoint_schema_conflict_and_atomic_outbox(self):
        self.assertIsNone(self.agent.get("thread:1"))
        created = self.agent.checkpoint("thread:1", {"messages": ["hello"]}, now_ms=1000,
                                        metadata={"owner": "ana"})
        self.assertTrue(created["applied"])
        checkpoint = created["checkpoint"]
        self.assertEqual(checkpoint, {
            "schema_version": 1, "thread_id": "thread:1", "revision": created["cursor"],
            "status": "running", "state": {"messages": ["hello"]},
            "metadata": {"owner": "ana"}, "updated_at": 1000,
        })
        before = self.db.stats()
        rejected = self.agent.checkpoint("thread:1", "stale", now_ms=1001, operations=[
            {"kind": "put", "collection": "outbox", "key": "rejected", "value": True},
        ])
        self.assertEqual(rejected, {"applied": False, "cursor": before["latest_cursor"],
                                    "checkpoint": checkpoint})
        self.assertEqual(self.db.stats(), before)
        self.assertIsNone(self.db.collection("outbox").get("rejected"))
        updated = self.agent.checkpoint(
            "thread:1", {"answer": "saved"}, expected_revision=checkpoint["revision"],
            status="completed", now_ms=1002, operations=[
                {"kind": "put", "collection": "outbox", "key": "answered", "value": {"exact": 2**64 - 1}},
            ],
        )
        self.assertTrue(updated["applied"])
        self.assertEqual(updated["checkpoint"]["revision"], updated["cursor"])
        self.assertGreater(updated["cursor"], checkpoint["revision"])
        transaction = self.db.changes(since=before["latest_cursor"])["transactions"]
        self.assertEqual(len(transaction), 1)
        self.assertEqual(len(transaction[0]["operations"]), 2)
        self.assertEqual(self.db.collection("outbox").get("answered"), {"exact": 2**64 - 1})
        stale = self.agent.checkpoint("thread:1", None, expected_revision=checkpoint["revision"])
        self.assertFalse(stale["applied"])
        self.assertEqual(stale["checkpoint"], updated["checkpoint"])

    def test_reopen_namespaces_null_state_and_monotonic_revision(self):
        first = self.agent.checkpoint("same", None, now_ms=1, status="waiting")
        self.db.agent("other").checkpoint("same", "independent", now_ms=1)
        self.db.collection("events").put("gap", True)
        self.db.close()
        self.db = Database(self.path)
        self.agent = self.db.agent("assistant")
        self.assertEqual(self.agent.get("same"), first["checkpoint"])
        self.assertEqual(self.db.agent("other").get("same")["state"], "independent")
        next_result = self.agent.checkpoint("same", True, expected_revision=first["cursor"], now_ms=2)
        self.assertTrue(next_result["applied"])
        self.assertGreater(next_result["cursor"], first["cursor"] + 1)

    def test_concurrent_updates_with_one_revision_have_one_winner(self):
        revision = self.agent.checkpoint("thread", {"count": 0})["cursor"]
        with ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(lambda number: self.agent.checkpoint(
                "thread", {"count": number}, expected_revision=revision,
            ), range(1, 9)))
        self.assertEqual(sum(result["applied"] for result in results), 1)
        self.assertEqual(self.db.stats()["latest_cursor"], revision + 1)

    def test_invalid_operations_roll_back_checkpoint_and_business_writes(self):
        original = self.agent.checkpoint("thread", "original")["checkpoint"]
        before = self.db.stats()
        invalid = [
            {"kind": "put", "collection": "", "key": "bad", "value": True},
            {"kind": "delete", "collection": "_cedro.agents.assistant", "key": "thread"},
            {"kind": "delete", "collection": "_cedro.jobs.queue", "key": "job"},
        ]
        for operation in invalid:
            with self.subTest(operation=operation):
                with self.assertRaises(CedroError):
                    self.agent.checkpoint("thread", "changed", expected_revision=original["revision"], operations=[
                        {"kind": "put", "collection": "business", "key": "first", "value": True},
                        operation,
                    ])
                self.assertEqual(self.agent.get("thread"), original)
                self.assertEqual(self.db.stats(), before)
                self.assertIsNone(self.db.collection("business").get("first"))

    def test_portable_json_and_strict_options_are_checked_without_mutation(self):
        before = self.db.stats()
        invalid = [
            lambda: self.db.agent("bad namespace"), lambda: self.db.agent("á"),
            lambda: self.agent.get(""), lambda: self.agent.get("a\x00b"),
            lambda: self.agent.checkpoint("t", {}, expected_revision=True),
            lambda: self.agent.checkpoint("t", {}, expected_revision=0),
            lambda: self.agent.checkpoint("t", {}, expected_revision=MAX_SAFE_INTEGER + 1),
            lambda: self.agent.checkpoint("t", {}, now_ms=True),
            lambda: self.agent.checkpoint("t", {}, now_ms=-1),
            lambda: self.agent.checkpoint("t", {}, status="unknown"),
            lambda: self.agent.checkpoint("t", {}, status=False),
            lambda: self.agent.checkpoint("t", {}, metadata=[]),
            lambda: self.agent.checkpoint("t", {"nested": [MAX_SAFE_INTEGER + 1]}),
            lambda: self.agent.checkpoint("t", {}, metadata={"n": -MAX_SAFE_INTEGER - 1}),
            lambda: self.agent.checkpoint("t", {"n": float("nan")}),
        ]
        for call in invalid:
            with self.assertRaises((TypeError, ValueError, OverflowError)):
                call()
            self.assertEqual(self.db.stats(), before)
        safe = self.agent.checkpoint("t", {"n": [-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER], "bool": True})
        self.assertTrue(safe["applied"])

    def test_native_bridge_is_strict_and_core_checks_portable_json(self):
        request = {"action": "checkpoint", "namespace": "assistant", "thread_id": "native",
                   "state": {}, "expected_revision": None, "now_ms": 1,
                   "status": "running", "metadata": {}, "operations": []}
        before = self.db.stats()
        with self.assertRaises(ValueError):
            self.db._native.agent_json(json.dumps(dict(request, typo=True)))
        with self.assertRaises(ValueError):
            self.db._native.agent_json(json.dumps(dict(request, action="missing")))
        with self.assertRaises(CedroError):
            self.db._native.agent_json(json.dumps(dict(request, state={"n": MAX_SAFE_INTEGER + 1})))
        self.assertEqual(self.db.stats(), before)

    def test_failed_revision_for_absent_thread_returns_none(self):
        result = self.agent.checkpoint("absent", {}, expected_revision=1)
        self.assertEqual(result, {"applied": False, "cursor": 0, "checkpoint": None})

    def test_checkpoint_is_included_in_backup_and_reserved_from_generic_writes(self):
        checkpoint = self.agent.checkpoint("thread", "restore")["checkpoint"]
        backup = Path(self.directory.name) / "snapshot.cedrobackup"
        restored = Path(self.directory.name) / "restored.cedro"
        self.db.backup(backup)
        Database.restore_backup(backup, restored)
        with Database(restored) as database:
            self.assertEqual(database.agent("assistant").get("thread"), checkpoint)
        for operation in [
            lambda: self.db.collection("_cedro.agents.assistant").put("thread", {}),
            lambda: self.db.collection("_cedro.agents.assistant").delete("thread"),
            lambda: self.db.collection("_cedro.agents.assistant").compare_exchange("thread", replacement={}),
        ]:
            with self.assertRaises(CedroError):
                operation()

    def test_handles_fail_after_close(self):
        self.db.close()
        for call in [lambda: self.db.agent("new"), lambda: self.agent.get("t"),
                     lambda: self.agent.checkpoint("t", {})]:
            with self.assertRaises(DatabaseClosedError):
                call()


if __name__ == "__main__":
    unittest.main()
