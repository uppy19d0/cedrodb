import math
from pathlib import Path
from tempfile import TemporaryDirectory
from concurrent.futures import ThreadPoolExecutor
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError, MISSING, DELETE
from cedrodb import _native


class DatabaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-test-")
        self.path = Path(self.directory.name) / "app.cedro"
        self.db = Database(self.path)
        self.orders = self.db.collection("orders")

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_real_native_extension(self):
        self.assertTrue(str(_native.__file__).endswith((".so", ".pyd")))

    def test_crud_and_reopen_preserve_arbitrary_json(self):
        values = [None, True, "á🌳", [1, {"nested": False}], {"total": 12.5}]
        self.orders.put_many({str(index): value for index, value in enumerate(values)})
        self.assertIs(self.orders.get("unknown", MISSING), MISSING)
        self.assertIsNone(self.orders.get("0", MISSING))
        self.orders.delete("1")
        self.db.close()
        self.db = Database(self.path)
        self.orders = self.db.collection("orders")
        for index, value in enumerate(values):
            if index != 1:
                self.assertEqual(self.orders.get(str(index), MISSING), value)
        self.assertIs(self.orders.get("1", MISSING), MISSING)

    def test_integer_boundaries_stay_exact_through_persistence_and_feed(self):
        values = {"min": -(2**63), "max": 2**64 - 1, "large": 2**53 + 1}
        self.orders.put("integers", values)
        self.assertEqual(self.orders.get("integers"), values)
        self.assertEqual(self.db.changes()["transactions"][0]["operations"][0]["value"], values)
        self.db.close()
        self.db = Database(self.path)
        self.orders = self.db.collection("orders")
        self.assertEqual(self.orders.get("integers"), values)

    def test_invalid_json_never_partially_commits_a_batch(self):
        cycle = []
        cycle.append(cycle)
        invalid = [math.nan, math.inf, -math.inf, 2**64, -(2**63) - 1, {1: "lossy"}, object(), cycle]
        initial = self.db.stats()
        for value in invalid:
            with self.subTest(value_type=type(value).__name__):
                with self.assertRaises((ValueError, TypeError, OverflowError)):
                    self.db.batch([
                        {"kind": "put", "collection": "orders", "key": "first", "value": "valid"},
                        {"kind": "put", "collection": "orders", "key": "bad", "value": {"nested": value}},
                    ])
                self.assertEqual(self.db.stats(), initial)
                self.assertIs(self.orders.get("first", MISSING), MISSING)

    def test_invalid_operations_roll_back_staged_writes(self):
        initial = self.db.stats()
        with self.assertRaises(CedroError):
            self.db.batch([
                {"kind": "put", "collection": "orders", "key": "valid", "value": 1},
                {"kind": "put", "collection": "", "key": "invalid", "value": 2},
            ])
        self.assertEqual(self.db.stats(), initial)
        self.assertIs(self.orders.get("valid", MISSING), MISSING)
        with self.assertRaises(ValueError):
            self.db.batch([
                {"kind": "put", "collection": "orders", "key": "valid", "value": 1},
                {"kind": "delete", "collection": "orders", "key": "invalid", "value": None},
            ])
        self.assertEqual(self.db.stats(), initial)

    def test_context_transaction_is_atomic_and_snapshots_values(self):
        document = {"total": 20}
        with self.db.transaction() as tx:
            tx.put("orders", "1", document)
            tx.put("outbox", "1", {"type": "created"})
            document["total"] = 999
            self.assertIs(self.orders.get("1", MISSING), MISSING)
        self.assertEqual(self.orders.get("1"), {"total": 20})
        self.assertEqual(self.db.stats()["latest_cursor"], 1)
        self.assertEqual(len(self.db.changes()["transactions"][0]["operations"]), 2)
        with self.assertRaisesRegex(RuntimeError, "abort"):
            with self.db.transaction() as tx:
                tx.put("orders", "2", {"total": 30})
                raise RuntimeError("abort")
        self.assertIs(self.orders.get("2", MISSING), MISSING)
        self.assertEqual(self.db.stats()["latest_cursor"], 1)
        with self.assertRaises(CedroError):
            tx.commit()

    def test_failed_transaction_commit_leaves_all_values_unchanged(self):
        with self.assertRaises(CedroError):
            with self.db.transaction() as tx:
                tx.put("orders", "1", True)
                tx.put("", "2", False)
        self.assertEqual(self.db.stats()["latest_cursor"], 0)
        self.assertIs(self.orders.get("1", MISSING), MISSING)

    def test_compare_exchange_distinguishes_missing_null_and_delete(self):
        self.assertTrue(self.orders.compare_exchange("nullable", expected=MISSING, replacement=None))
        cursor = self.db.stats()["latest_cursor"]
        self.assertFalse(self.orders.compare_exchange("nullable", expected=MISSING, replacement=1))
        self.assertEqual(self.db.stats()["latest_cursor"], cursor)
        self.assertIsNone(self.orders.get("nullable", MISSING))
        self.assertTrue(self.orders.compare_exchange("nullable", expected=None, replacement={"state": "ready"}))
        self.assertTrue(self.orders.compare_exchange("nullable", expected={"state": "ready"}, replacement=DELETE))
        self.assertIs(self.orders.get("nullable", MISSING), MISSING)
        with self.assertRaises(TypeError):
            self.orders.compare_exchange("nullable", expected=MISSING)
        self.assertTrue(self.orders.compare_exchange("absent", expected=MISSING, replacement=DELETE))

    def test_query_pagination_and_index_updates(self):
        self.orders.put_many({
            "a": {"status": "paid", "amount": 10, "customer": {"city": "SD"}},
            "b": {"status": "paid", "amount": 20, "customer": {"city": "SD"}},
            "c": {"status": "pending", "amount": 30},
            "d": {"status": "paid", "amount": 40, "customer": {"city": "NY"}},
        })
        filters = [{"field": "status", "op": "eq", "value": "paid"}]
        self.orders.create_index("status")
        first = self.orders.find_page(filters, limit=2)
        self.assertEqual([record["key"] for record in first["records"]], ["a", "b"])
        self.assertEqual(first["next_after"], "b")
        self.assertEqual(first["used_index"], "status")
        second = self.orders.find_page(filters, limit=2, after=first["next_after"])
        self.assertEqual([record["key"] for record in second["records"]], ["d"])
        self.assertIsNone(second["next_after"])
        self.assertEqual([r["key"] for r in self.orders.iter_records(filters, page_size=1)], ["a", "b", "d"])
        self.orders.put("c", {"status": "paid", "amount": 30})
        self.orders.delete("a")
        self.assertEqual([r["key"] for r in self.orders.find(filters)], ["b", "c", "d"])
        nested = self.orders.find([{"field": "/customer/city", "op": "eq", "value": "SD"}])
        self.assertEqual([r["key"] for r in nested], ["b"])
        missing = self.orders.find([{"field": "customer", "op": "exists", "value": False}])
        self.assertEqual([r["key"] for r in missing], ["c"])

    def test_ordered_integer_query_does_not_round_large_integers(self):
        self.orders.put_many({"a": {"n": 2**64 - 2}, "b": {"n": 2**64 - 1}})
        records = self.orders.find([{"field": "n", "op": "gt", "value": 2**64 - 2}])
        self.assertEqual([r["key"] for r in records], ["b"])
        with self.assertRaises(CedroError):
            self.orders.find([{"field": "n", "op": "gt", "value": 1.5}])

    def test_safe_numeric_equality_interoperates_without_coercing_booleans(self):
        self.orders.put_many({"one": {"n": 1.0}, "bool": {"n": True}, "huge": {"n": 2**53 + 1}})
        self.orders.create_index("n")
        self.assertEqual(
            [record["key"] for record in self.orders.find([{"field": "n", "op": "eq", "value": 1}])],
            ["one"],
        )
        self.assertTrue(self.orders.compare_exchange("one", expected={"n": 1}, replacement={"n": [1.0, True]}))
        self.assertTrue(self.orders.compare_exchange("one", expected={"n": [1, True]}, replacement=None))
        self.assertFalse(self.orders.compare_exchange("bool", expected={"n": 1}, replacement=None))
        self.assertFalse(self.orders.compare_exchange("huge", expected={"n": float(2**53 + 1)}, replacement=None))

    def test_changefeed_resumes_atomic_outbox_after_reopen(self):
        with self.db.transaction() as tx:
            tx.put("orders", "1", {"paid": True})
            tx.put("outbox", "event:1", {"event_id": "event:1"})
        self.orders.delete("1")
        page = self.db.changes(limit=1)
        self.assertEqual(len(page["transactions"]), 1)
        self.assertEqual(len(page["transactions"][0]["operations"]), 2)
        self.assertTrue(page["has_more"])
        self.assertEqual(page["next_cursor"], 1)
        self.db.close()
        self.db = Database(self.path)
        self.orders = self.db.collection("orders")
        next_page = self.db.changes(since=page["next_cursor"])
        self.assertEqual(next_page["transactions"], [{
            "cursor": 2, "operations": [{"kind": "delete", "collection": "orders", "key": "1"}]
        }])
        self.assertFalse(next_page["has_more"])
        self.assertEqual(self.db.changes(since=2)["transactions"], [])

    def test_close_releases_lock_even_with_existing_collection(self):
        with self.assertRaises(CedroError):
            Database(self.path)
        self.db.close()
        self.assertTrue(self.db.closed)
        self.db.close()
        for call in [lambda: self.orders.get("1"), self.db.stats, self.db.changes,
                     lambda: self.db.collection("new"), lambda: self.orders.find()]:
            with self.assertRaises(DatabaseClosedError):
                call()
        with Database(self.path) as reopened:
            reopened.collection("orders").put("1", "lock released")

    def test_database_context_cleans_up_when_user_raises(self):
        self.db.close()
        with self.assertRaisesRegex(RuntimeError, "user error"):
            with Database(self.path) as current:
                current.collection("orders").put("1", True)
                raise RuntimeError("user error")
        self.assertTrue(current.closed)
        with Database(self.path) as reopened:
            self.assertTrue(reopened.collection("orders").get("1"))

    def test_threaded_native_writes_are_serialized(self):
        def write(index):
            self.orders.put(str(index), {"n": index})

        with ThreadPoolExecutor(max_workers=4) as executor:
            list(executor.map(write, range(16)))
        self.assertEqual(self.db.stats()["records"], 16)
        self.assertEqual(self.db.stats()["latest_cursor"], 16)

    def test_validation_limits_and_empty_batch(self):
        self.db.batch([])
        self.assertEqual(self.db.stats()["latest_cursor"], 0)
        with self.assertRaises(CedroError):
            self.orders.find(limit=1001)
        with self.assertRaises(ValueError):
            self.orders.find(limit=0)
        with self.assertRaises(TypeError):
            self.db.changes(since=True)
        with self.assertRaises(CedroError):
            self.db.changes(since=1)
        with self.assertRaises(CedroError):
            self.orders.find([{"field": "a", "op": "exists", "value": "yes"}])

    def test_document_limit_rejects_entire_batch(self):
        self.db.close()
        self.db = Database(self.path, max_document_bytes=64)
        self.orders = self.db.collection("orders")
        with self.assertRaises(CedroError):
            self.orders.put_many({"small": 1, "large": "x" * 100})
        self.assertEqual(self.db.stats()["records"], 0)
        self.assertEqual(self.db.stats()["latest_cursor"], 0)


if __name__ == "__main__":
    unittest.main()
