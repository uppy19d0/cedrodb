from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from cedrodb import Database, CedroError, DatabaseClosedError, MAX_SAFE_INTEGER


class QueueTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-queue-")
        self.root = Path(self.directory.name)
        self.path = self.root / "worker.cedro"
        self.db = Database(self.path)
        self.queue = self.db.queue("emails")

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_enqueue_deduplicates_ids_and_keeps_job_schema(self):
        result = self.queue.enqueue("mail:1", {"to": "person@example.test"}, now_ms=1000, priority=5)
        self.assertTrue(result["inserted"])
        job = result["job"]
        self.assertEqual(job["schema_version"], 1)
        self.assertEqual(job["status"], "queued")
        self.assertEqual((job["created_at"], job["available_at"], job["attempts"], job["max_attempts"]), (1000, 1000, 0, 3))
        self.assertIsNone(job["lease_until"])
        duplicate = self.queue.enqueue("mail:1", {"different": True}, now_ms=2000)
        self.assertFalse(duplicate["inserted"])
        self.assertEqual(duplicate["job"], job)
        self.assertEqual(self.queue.get("mail:1"), job)
        self.assertIsNone(self.queue.get("unknown"))
        self.assertEqual(self.queue.stats(now_ms=1000), {
            "queued": 1, "leased": 0, "done": 0, "dead": 0, "ready": 1, "expired": 0,
        })
        other = self.db.queue("other")
        self.assertTrue(other.enqueue("mail:1", "independent", now_ms=1000)["inserted"])
        self.assertEqual(other.get("mail:1")["payload"], "independent")

    def test_priority_and_delayed_availability(self):
        self.queue.enqueue("low", {}, now_ms=1000, priority=0)
        self.queue.enqueue("high", {}, now_ms=1000, priority=50)
        self.queue.enqueue("later", {}, now_ms=1000, delay_ms=50, priority=100)
        self.assertEqual(self.queue.claim("worker", now_ms=1000, lease_ms=10000)["id"], "high")
        self.assertEqual(self.queue.claim("worker", now_ms=1000, lease_ms=10000)["id"], "low")
        self.assertIsNone(self.queue.claim("worker", now_ms=1049))
        self.assertEqual(self.queue.claim("worker", now_ms=1050)["id"], "later")

    def test_lease_extension_reclaim_and_stale_token_fencing(self):
        self.queue.enqueue("job", {}, now_ms=1000)
        first = self.queue.claim("worker-1", now_ms=1000, lease_ms=10)
        token = first["lease_token"]
        self.assertEqual((first["attempts"], first["lease_until"], first["worker"]), (1, 1010, "worker-1"))
        self.assertFalse(self.queue.ack("job", "wrong", now_ms=1001))
        self.assertFalse(self.queue.fail("job", "wrong", "error", now_ms=1001))
        self.assertFalse(self.queue.extend("job", "wrong", now_ms=1001))
        self.assertTrue(self.queue.extend("job", token, now_ms=1002, lease_ms=20))
        self.assertEqual(self.queue.get("job")["lease_until"], 1022)
        self.assertIsNone(self.queue.claim("worker-2", now_ms=1021))
        second = self.queue.claim("worker-2", now_ms=1022, lease_ms=10)
        self.assertEqual(second["attempts"], 2)
        self.assertNotEqual(second["lease_token"], token)
        self.assertFalse(self.queue.ack("job", token, now_ms=1023))
        self.assertFalse(self.queue.extend("job", token, now_ms=1023))
        self.assertTrue(self.queue.ack("job", second["lease_token"], now_ms=1023))
        self.assertEqual(self.queue.get("job")["status"], "done")
        self.assertFalse(self.queue.ack("job", second["lease_token"], now_ms=1024))
        self.assertFalse(self.queue.enqueue("job", {"new": True}, now_ms=1024)["inserted"])

    def test_failed_attempt_retries_then_becomes_dead_and_can_be_reset(self):
        self.queue.enqueue("job", {}, now_ms=1000, max_attempts=2)
        first = self.queue.claim("worker", now_ms=1000, lease_ms=100)
        self.assertTrue(self.queue.fail("job", first["lease_token"], "temporary error", now_ms=1001, retry_delay_ms=50))
        waiting = self.queue.get("job")
        self.assertEqual((waiting["status"], waiting["available_at"], waiting["last_error"]), ("queued", 1051, "temporary error"))
        self.assertIsNone(self.queue.claim("worker", now_ms=1050))
        second = self.queue.claim("worker", now_ms=1051, lease_ms=100)
        self.assertEqual(second["attempts"], 2)
        self.assertTrue(self.queue.fail("job", second["lease_token"], "exhausted", now_ms=1052))
        self.assertEqual(self.queue.get("job")["status"], "dead")
        self.assertIsNone(self.queue.claim("worker", now_ms=1053))
        self.assertFalse(self.queue.enqueue("job", "new", now_ms=1053)["inserted"])
        self.assertTrue(self.queue.retry_dead("job", now_ms=1053))
        self.assertEqual((self.queue.get("job")["status"], self.queue.get("job")["attempts"]), ("queued", 0))
        self.assertFalse(self.queue.retry_dead("job", now_ms=1053))

    def test_expired_recovery_is_bounded_and_marks_exhausted_jobs_dead(self):
        for index in range(3):
            self.queue.enqueue(str(index), {}, now_ms=1000, max_attempts=1)
            self.assertIsNotNone(self.queue.claim("worker", now_ms=1000, lease_ms=10))
        self.assertEqual(self.queue.stats(now_ms=1010)["expired"], 3)
        first = self.queue.recover_expired(now_ms=1010, limit=2)
        self.assertEqual(first, {"requeued": 0, "dead": 2, "has_more": True})
        self.assertEqual(self.queue.recover_expired(now_ms=1010, limit=2), {"requeued": 0, "dead": 1, "has_more": False})
        self.assertEqual(self.queue.stats(now_ms=1010)["dead"], 3)
        self.queue.enqueue("retryable", {}, now_ms=1000, max_attempts=2)
        self.queue.claim("worker", now_ms=1000, lease_ms=10)
        self.assertEqual(self.queue.recover_expired(now_ms=1010), {"requeued": 1, "dead": 0, "has_more": False})
        self.assertEqual(self.queue.claim("worker", now_ms=1010)["attempts"], 2)

    def test_ack_and_business_outbox_writes_commit_together_with_exact_integers(self):
        self.queue.enqueue("report", {"order_id": "1"}, now_ms=1000)
        claimed = self.queue.claim("worker", now_ms=1000, lease_ms=100)
        before = self.db.stats()["latest_cursor"]
        self.assertTrue(self.queue.ack("report", claimed["lease_token"], now_ms=1001, operations=[
            {"kind": "put", "collection": "reports", "key": "1", "value": {"exact": 2**64 - 1}},
            {"kind": "put", "collection": "outbox", "key": "report:1", "value": {"event": "report.ready"}},
        ]))
        self.assertEqual(self.db.stats()["latest_cursor"], before + 1)
        self.assertEqual(self.db.collection("reports").get("1"), {"exact": 2**64 - 1})
        self.assertEqual(self.queue.get("report")["status"], "done")
        changes = self.db.changes(since=before)["transactions"]
        self.assertEqual(len(changes), 1)
        self.assertEqual(len(changes[0]["operations"]), 3)
        self.assertEqual({op["collection"] for op in changes[0]["operations"]}, {"reports", "outbox", "_cedro.jobs.emails"})

    def test_invalid_ack_never_marks_done_or_applies_partial_business_writes(self):
        self.queue.enqueue("job", {}, now_ms=1000)
        token = self.queue.claim("worker", now_ms=1000, lease_ms=100)["lease_token"]
        before = self.db.stats()
        for bad in [
            {"kind": "put", "collection": "", "key": "invalid", "value": 1},
            {"kind": "delete", "collection": "_cedro.jobs.other", "key": "job"},
            {"kind": "put", "collection": "business", "key": "nan", "value": float("nan")},
        ]:
            with self.subTest(operation=bad):
                with self.assertRaises((CedroError, ValueError)):
                    self.queue.ack("job", token, now_ms=1001, operations=[
                        {"kind": "put", "collection": "business", "key": "valid", "value": True}, bad,
                    ])
                self.assertEqual(self.db.stats(), before)
                self.assertEqual(self.queue.get("job")["status"], "leased")
                self.assertIsNone(self.db.collection("business").get("valid"))

    def test_queue_payload_portability_is_checked_before_native_mutation(self):
        self.queue.enqueue("safe", {"bounds": [-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER]}, now_ms=1000)
        before = self.db.stats()
        for value in [MAX_SAFE_INTEGER + 1, -MAX_SAFE_INTEGER - 1, float(MAX_SAFE_INTEGER * 2), float("inf")]:
            with self.subTest(value=value):
                with self.assertRaises((OverflowError, ValueError)):
                    self.queue.enqueue("unsafe", {"nested": [value]}, now_ms=1000)
                self.assertEqual(self.db.stats(), before)
        request = {"action": "enqueue", "name": "emails", "id": "unsafe-native",
                   "payload": {"nested": [MAX_SAFE_INTEGER + 1]}, "now_ms": 1000,
                   "delay_ms": 0, "max_attempts": 3, "priority": 0}
        with self.assertRaises(CedroError):
            self.db._native.queue_json(json.dumps(request))
        self.assertEqual(self.db.stats(), before)
        self.db.collection("ordinary").put("exact", {"number": 2**64 - 1})
        self.assertEqual(self.db.collection("ordinary").get("exact")["number"], 2**64 - 1)

    def test_clock_options_names_and_identifiers_are_strict(self):
        self.queue.enqueue("job", {}, now_ms=1000)
        token = self.queue.claim("worker", now_ms=1000, lease_ms=100)["lease_token"]
        before = self.db.stats()
        invalid = [
            lambda: self.db.queue("bad name"), lambda: self.db.queue("á"), lambda: self.db.queue("x" * 129),
            lambda: self.queue.enqueue("", {}, now_ms=1000),
            lambda: self.queue.enqueue("bad\0id", {}, now_ms=1000),
            lambda: self.queue.enqueue("é" * 2049, {}, now_ms=1000),
            lambda: self.queue.enqueue("new", {}, now_ms=True),
            lambda: self.queue.enqueue("new", {}, now_ms=MAX_SAFE_INTEGER + 1),
            lambda: self.queue.enqueue("new", {}, now_ms=1000, delay_ms=-1),
            lambda: self.queue.enqueue("new", {}, now_ms=1000, max_attempts=True),
            lambda: self.queue.enqueue("new", {}, now_ms=1000, max_attempts=0),
            lambda: self.queue.enqueue("new", {}, now_ms=1000, priority=1001),
            lambda: self.queue.claim("é" * 65, now_ms=1000),
            lambda: self.queue.claim("worker", now_ms=1000, lease_ms=0),
            lambda: self.queue.ack("job", "", now_ms=1001),
            lambda: self.queue.fail("job", token, "", now_ms=1001),
            lambda: self.queue.fail("job", token, "bad\0error", now_ms=1001),
            lambda: self.queue.fail("job", token, "é" * 2049, now_ms=1001),
            lambda: self.queue.extend("job", token, now_ms=1001, lease_ms=True),
            lambda: self.queue.recover_expired(now_ms=1000, limit=1001),
        ]
        for call in invalid:
            with self.assertRaises((TypeError, ValueError)):
                call()
            self.assertEqual(self.db.stats(), before)
        request = {"action": "stats", "name": "emails", "now_ms": 1000, "extra": True}
        with self.assertRaises(ValueError):
            self.db._native.queue_json(json.dumps(request))
        self.assertEqual(self.db.stats(), before)

    def test_leased_jobs_survive_reopen_and_backup_restore(self):
        self.queue.enqueue("job", {"task": "upload"}, now_ms=1000)
        leased = self.queue.claim("worker", now_ms=1000, lease_ms=10)
        backup = self.root / "worker.backup"
        self.db.backup(backup)
        self.db.close()
        self.db = Database(self.path)
        self.queue = self.db.queue("emails")
        self.assertEqual(self.queue.get("job"), leased)
        self.assertTrue(self.queue.ack("job", leased["lease_token"], now_ms=1001))
        destination = self.root / "restored.cedro"
        Database.restore_backup(backup, destination)
        with Database(destination) as restored:
            queue = restored.queue("emails")
            self.assertEqual(queue.get("job"), leased)
            self.assertEqual(queue.recover_expired(now_ms=1010)["requeued"], 1)
            self.assertEqual(queue.get("job")["status"], "queued")

    def test_concurrent_claims_only_lease_one_job(self):
        self.queue.enqueue("job", {}, now_ms=1000)
        with ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(
                lambda index: self.queue.claim("worker:" + str(index), now_ms=1000, lease_ms=100), range(8)
            ))
        self.assertEqual(sum(job is not None for job in results), 1)
        self.assertEqual(self.queue.get("job")["attempts"], 1)

    def test_default_clock_and_close_behavior(self):
        with patch("cedrodb.time.time_ns", return_value=2_000_000_000):
            self.assertEqual(self.queue.enqueue("job", {})["job"]["created_at"], 2000)
            claimed = self.queue.claim("worker")
            self.assertEqual(claimed["lease_until"], 32000)
            self.assertEqual(self.queue.stats()["leased"], 1)
        self.db.close()
        for call in [lambda: self.queue.get("job"), lambda: self.queue.stats(now_ms=2000),
                     lambda: self.queue.claim("worker", now_ms=2000), lambda: self.db.queue("new")]:
            with self.assertRaises(DatabaseClosedError):
                call()

    def test_reserved_queue_namespace_rejects_generic_writes_but_allows_inspection(self):
        job = self.queue.enqueue("job", {"task": "upload"}, now_ms=1000)["job"]
        reserved_name = "_cedro.jobs.emails"
        records = self.db.collection(reserved_name)
        self.assertEqual(records.get("job"), job)
        self.assertEqual(records.find(), [{"key": "job", "value": job}])
        before = self.db.stats()
        operations = [
            {"kind": "put", "collection": "business", "key": "prefix", "value": True},
            {"kind": "delete", "collection": reserved_name, "key": "job"},
        ]
        invalid = [
            lambda: records.put("new", {}),
            lambda: records.delete("job"),
            lambda: records.compare_exchange("job", expected=job, replacement=None),
            lambda: self.db.batch(operations),
            lambda: self.db.conditional_batch([], operations),
        ]
        for call in invalid:
            with self.assertRaises(CedroError):
                call()
            self.assertEqual(self.db.stats(), before)
            self.assertEqual(self.queue.get("job"), job)
            self.assertIsNone(self.db.collection("business").get("prefix"))
        with self.assertRaises(CedroError):
            with self.db.transaction() as tx:
                tx.put("business", "prefix", True)
                tx.delete(reserved_name, "job")
        self.assertEqual(self.db.stats(), before)
        checked = self.db.conditional_batch([
            {"kind": "equals", "collection": reserved_name, "key": "job", "value": job}
        ], [])
        self.assertEqual(checked, {"applied": True, "failed_condition": None, "cursor": before["latest_cursor"]})


if __name__ == "__main__":
    unittest.main()
