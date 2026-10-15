from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError, MISSING


class ConditionalBatchTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-conditional-")
        self.path = Path(self.directory.name) / "shop.cedro"
        self.db = Database(self.path)
        self.inventory = self.db.collection("inventory")
        self.inventory.put("desk", {"available": 10})

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def reservation(self, event="order:1"):
        return ([
            {"kind": "equals", "collection": "inventory", "key": "desk", "value": {"available": 10}},
            {"kind": "absent", "collection": "outbox", "key": event},
        ], [
            {"kind": "put", "collection": "inventory", "key": "desk", "value": {"available": 9}},
            {"kind": "put", "collection": "outbox", "key": event, "value": {"event_id": event}},
        ])

    def test_inventory_and_outbox_commit_in_one_frame_with_consistent_index(self):
        self.inventory.create_index("available")
        before = self.db.stats()["latest_cursor"]
        result = self.db.conditional_batch(*self.reservation())
        self.assertEqual(result, {"applied": True, "failed_condition": None, "cursor": before + 1})
        self.assertEqual(self.inventory.get("desk"), {"available": 9})
        old = self.inventory.find([{"field": "available", "op": "eq", "value": 10}])
        new = self.inventory.find_page([{"field": "available", "op": "eq", "value": 9}])
        self.assertEqual(old, [])
        self.assertEqual([record["key"] for record in new["records"]], ["desk"])
        self.assertEqual(new["used_index"], "available")
        changes = self.db.changes(since=before)["transactions"]
        self.assertEqual(len(changes), 1)
        self.assertEqual(len(changes[0]["operations"]), 2)
        self.assertEqual(changes[0]["cursor"], result["cursor"])
        self.db.close()
        self.db = Database(self.path)
        self.inventory = self.db.collection("inventory")
        self.assertEqual(self.db.changes(since=before)["transactions"], changes)
        self.assertEqual(self.db.verify()["transactions"], result["cursor"])

    def test_failed_condition_leaves_documents_log_and_cursor_unchanged(self):
        self.db.collection("outbox").put("order:1", {"already": True})
        before = self.db.stats()
        result = self.db.conditional_batch(*self.reservation())
        self.assertEqual(result, {"applied": False, "failed_condition": 1, "cursor": before["latest_cursor"]})
        self.assertEqual(self.db.stats(), before)
        self.assertEqual(self.inventory.get("desk"), {"available": 10})
        self.assertEqual(self.db.collection("outbox").get("order:1"), {"already": True})

    def test_absent_is_distinct_from_equals_null(self):
        nullable = self.db.collection("nullable")
        nullable.put("null", None)
        checks = [
            ({"kind": "absent", "collection": "nullable", "key": "null"}, False),
            ({"kind": "equals", "collection": "nullable", "key": "missing", "value": None}, False),
            ({"kind": "equals", "collection": "nullable", "key": "null", "value": None}, True),
            ({"kind": "absent", "collection": "nullable", "key": "missing"}, True),
        ]
        cursor = self.db.stats()["latest_cursor"]
        for condition, applied in checks:
            with self.subTest(condition=condition):
                result = self.db.conditional_batch([condition], [])
                self.assertIs(result["applied"], applied)
                self.assertEqual(result["cursor"], cursor)

    def test_all_conditions_observe_same_pre_state(self):
        result = self.db.conditional_batch([
            {"kind": "equals", "collection": "inventory", "key": "desk", "value": {"available": 10}},
            {"kind": "absent", "collection": "inventory", "key": "new"},
        ], [
            {"kind": "put", "collection": "inventory", "key": "desk", "value": {"available": 8}},
            {"kind": "put", "collection": "inventory", "key": "new", "value": True},
        ])
        self.assertTrue(result["applied"])
        self.assertEqual(self.inventory.get("desk"), {"available": 8})
        self.assertTrue(self.inventory.get("new"))

    def test_invalid_condition_shapes_and_json_never_mutate(self):
        conditions, operations = self.reservation()
        invalid = [
            {"kind": "absent", "collection": "outbox", "key": "order:1", "value": None},
            {"kind": "equals", "collection": "inventory", "key": "desk"},
            {"kind": "equals", "collection": "inventory", "key": "desk", "value": 2**64},
            {"kind": "equals", "collection": "inventory", "key": "desk", "value": float("nan")},
            {"kind": "absent", "collection": "", "key": "order:1"},
            {"kind": "unknown", "collection": "outbox", "key": "order:1"},
        ]
        before = self.db.stats()
        for condition in invalid:
            with self.subTest(condition=condition):
                with self.assertRaises((CedroError, ValueError, TypeError, OverflowError)):
                    self.db.conditional_batch([condition], operations)
                self.assertEqual(self.db.stats(), before)
        with self.assertRaises(ValueError):
            self.db._native.conditional_batch_json('{"conditions":[],"operations":[],"unknown":true}')
        self.assertEqual(self.db.stats(), before)

    def test_invalid_operation_is_rejected_even_if_condition_would_fail(self):
        before = self.db.stats()
        with self.assertRaises((CedroError, ValueError)):
            self.db.conditional_batch([
                {"kind": "equals", "collection": "inventory", "key": "desk", "value": "wrong"}
            ], [
                {"kind": "put", "collection": "inventory", "key": "desk", "value": 1},
                {"kind": "put", "collection": "", "key": "invalid", "value": 2},
            ])
        self.assertEqual(self.db.stats(), before)
        with self.assertRaises((ValueError, TypeError, OverflowError)):
            conditions, operations = self.reservation()
            operations.append({"kind": "put", "collection": "outbox", "key": "bad", "value": 2**64})
            self.db.conditional_batch(conditions, operations)
        self.assertEqual(self.db.stats(), before)

    def test_condition_count_and_document_limits_are_atomic(self):
        condition = {"kind": "absent", "collection": "outbox", "key": "event"}
        before = self.db.stats()
        with self.assertRaises(CedroError):
            self.db.conditional_batch([condition] * 65, self.reservation()[1])
        self.assertEqual(self.db.stats(), before)
        self.db.close()
        self.db = Database(self.path, max_document_bytes=64)
        before = self.db.stats()
        with self.assertRaises(CedroError):
            self.db.conditional_batch([
                {"kind": "equals", "collection": "inventory", "key": "desk", "value": "x" * 100}
            ], self.reservation()[1])
        self.assertEqual(self.db.stats(), before)

    def test_numeric_conditions_preserve_exactness_and_boolean_types(self):
        self.db.collection("numbers").put("n", {"values": [1.0, 2**64 - 1], "flag": True})
        condition = {"kind": "equals", "collection": "numbers", "key": "n",
                     "value": {"values": [1, 2**64 - 1], "flag": True}}
        self.assertTrue(self.db.conditional_batch([condition], [
            {"kind": "delete", "collection": "numbers", "key": "n"}
        ])["applied"])
        self.db.collection("numbers").put("n", {"flag": True})
        condition["value"] = {"flag": 1}
        self.assertFalse(self.db.conditional_batch([condition], [])["applied"])

    def test_oversized_operation_frame_does_not_commit_a_prefix(self):
        with Database(Path(self.directory.name) / "bounded.cedro", max_frame_bytes=128) as bounded:
            before = bounded.stats()
            with self.assertRaises(CedroError):
                bounded.conditional_batch([], [
                    {"kind": "put", "collection": "items", "key": "small", "value": 1},
                    {"kind": "put", "collection": "items", "key": "large", "value": "x" * 100},
                ])
            self.assertEqual(bounded.stats(), before)
            self.assertIs(bounded.collection("items").get("small", MISSING), MISSING)

    def test_competing_reservations_apply_only_once(self):
        def reserve(index):
            return self.db.conditional_batch(*self.reservation("order:" + str(index)))

        with ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(reserve, range(8)))
        self.assertEqual(sum(result["applied"] for result in results), 1)
        self.assertEqual(self.inventory.get("desk"), {"available": 9})
        self.assertEqual(len(self.db.collection("outbox").find()), 1)

    def test_closed_database_rejects_conditional_batch(self):
        self.db.close()
        with self.assertRaises(DatabaseClosedError):
            self.db.conditional_batch([], [])


if __name__ == "__main__":
    unittest.main()
