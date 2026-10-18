from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError, ERROR_CODES


class ErrorCodeTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-codes-")
        self.path = Path(self.directory.name) / "app.cedro"

    def tearDown(self):
        self.directory.cleanup()

    def test_codes_are_stable_strings(self):
        self.assertEqual(len(set(ERROR_CODES)), len(ERROR_CODES))
        self.assertTrue(all(code.startswith("CEDRO_") for code in ERROR_CODES))

    def test_use_after_close_is_coded_and_keeps_type(self):
        db = Database(self.path)
        orders = db.collection("orders")
        db.close()
        for call in (lambda: orders.get("1"), db.stats):
            with self.assertRaises(DatabaseClosedError) as raised:
                call()
            self.assertEqual(raised.exception.code, "CEDRO_CLOSED")
            self.assertIn("closed", str(raised.exception))

    def test_locked_database(self):
        with Database(self.path):
            with self.assertRaises(CedroError) as raised:
                Database(self.path)
        self.assertEqual(raised.exception.code, "CEDRO_DATABASE_LOCKED")

    def test_limit_exceeded(self):
        with Database(self.path, max_document_bytes=30) as db:
            with self.assertRaises(CedroError) as raised:
                db.collection("a").put("k", {"text": "x" * 100})
        self.assertEqual(raised.exception.code, "CEDRO_LIMIT_EXCEEDED")

    def test_invalid_input_from_engine(self):
        with Database(self.path) as db:
            with self.assertRaises(CedroError) as raised:
                db.collection("_cedro.jobs.x").put("k", 1)
        self.assertEqual(raised.exception.code, "CEDRO_INVALID_INPUT")

    def test_io_error(self):
        blocker = Path(self.directory.name) / "plain-file"
        blocker.write_text("x")
        with self.assertRaises(CedroError) as raised:
            Database(blocker / "child")
        self.assertEqual(raised.exception.code, "CEDRO_IO")

    def test_corruption(self):
        with Database(self.path) as db:
            db.collection("a").put("k", 1)
        target = self.path if self.path.is_file() else next(
            p for p in self.path.iterdir() if p.suffix in (".log", ".cedro"))
        data = bytearray(target.read_bytes())
        data[0] ^= 0xFF
        target.write_bytes(bytes(data))
        with self.assertRaises(CedroError) as raised:
            Database(self.path)
        self.assertEqual(raised.exception.code, "CEDRO_CORRUPTION")


if __name__ == "__main__":
    unittest.main()
