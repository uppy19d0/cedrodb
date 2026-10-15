from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from cedrodb import Database, CedroError, DatabaseClosedError, MISSING


class BackupTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory(prefix="cedrodb-python-backup-")
        self.root = Path(self.directory.name)
        self.path = self.root / "forms.cedro"
        self.backup_path = self.root / "forms.backup"
        self.destination = self.root / "restored.cedro"
        self.db = Database(self.path)
        with self.db.transaction() as tx:
            tx.put("forms", "form:1", {"answers": [True, None, 2**64 - 1], "status": "pending"})
            tx.put("outbox", "form:1:submitted", {"type": "form.submitted", "id": "form:1"})
        self.db.collection("forms").put("discarded", {"temporary": True})
        self.db.collection("forms").delete("discarded")

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def test_verify_reports_persisted_history_without_mutation(self):
        before = self.db.stats()
        report = self.db.verify()
        self.assertEqual(set(report), {"log_bytes", "transactions", "records", "content_bytes"})
        self.assertEqual(report["log_bytes"], before["log_bytes"])
        self.assertEqual(report["transactions"], before["latest_cursor"])
        self.assertEqual(report["records"], before["records"])
        self.assertEqual(report["content_bytes"], before["content_bytes"])
        self.assertEqual(self.db.stats(), before)

    def test_backup_restores_point_in_time_documents_and_complete_changefeed(self):
        original_feed = self.db.changes()
        self.db.collection("forms").create_index("status")
        report = self.db.backup(self.backup_path)
        self.assertEqual(report, self.db.verify())
        self.db.collection("forms").put("later", {"status": "pending"})
        restored_report = Database.restore_backup(self.backup_path, self.destination)
        self.assertEqual(restored_report, report)
        # Static restore keeps no lock or open handle for the destination.
        with Database(self.destination) as restored:
            self.assertEqual(restored.verify(), report)
            self.assertEqual(restored.changes(), original_feed)
            self.assertIs(restored.collection("forms").get("later", MISSING), MISSING)
            self.assertEqual(restored.stats()["indexes"], 0)
            self.assertEqual(
                restored.collection("forms").get("form:1")["answers"], [True, None, 2**64 - 1]
            )
            restored.collection("forms").create_index("status")
            self.assertEqual(restored.stats()["indexes"], 1)

    def test_backup_refuses_to_overwrite_existing_file(self):
        self.db.backup(self.backup_path)
        contents = self.backup_path.read_bytes()
        self.db.collection("forms").put("later", True)
        with self.assertRaises(CedroError):
            self.db.backup(self.backup_path)
        self.assertEqual(self.backup_path.read_bytes(), contents)
        with Database(self.root / "empty.cedro") as empty:
            with self.assertRaises(CedroError):
                empty.backup(self.path)
        self.assertTrue((self.path / "cedro.log").is_file())

    def test_restore_rejects_existing_directory_without_overwriting_files(self):
        self.db.backup(self.backup_path)
        self.destination.mkdir()
        with self.assertRaises(CedroError):
            Database.restore_backup(self.backup_path, self.destination)
        marker = self.destination / "keep.txt"
        marker.write_text("keep", encoding="utf-8")
        with self.assertRaises(CedroError):
            Database.restore_backup(self.backup_path, self.destination)
        self.assertEqual(marker.read_text(encoding="utf-8"), "keep")
        self.assertEqual(list(self.destination.iterdir()), [marker])

    def test_corrupt_or_truncated_backup_does_not_create_destination(self):
        self.db.backup(self.backup_path)
        original = self.backup_path.read_bytes()
        invalid = [original[:-1], original + b"extra", bytes([original[0] ^ 1]) + original[1:]]
        corrupted = bytearray(original)
        corrupted[-1] ^= 0x80
        invalid.append(bytes(corrupted))
        for index, contents in enumerate(invalid):
            with self.subTest(corruption=index):
                damaged = self.root / ("damaged-" + str(index))
                damaged.write_bytes(contents)
                with self.assertRaises(CedroError):
                    Database.restore_backup(damaged, self.destination)
                self.assertFalse(self.destination.exists())

    def test_restore_validates_content_limits_before_creating_destination(self):
        self.db.backup(self.backup_path)
        for options in [{"max_document_bytes": 8}, {"max_state_bytes": 1}, {"max_frame_bytes": 10}]:
            with self.subTest(options=options):
                with self.assertRaises(CedroError):
                    Database.restore_backup(self.backup_path, self.destination, **options)
                self.assertFalse(self.destination.exists())
        report = Database.restore_backup(
            self.backup_path, self.destination, max_document_bytes=1024,
            max_frame_bytes=4096, max_state_bytes=8192,
        )
        with Database(self.destination, max_document_bytes=1024, max_frame_bytes=4096, max_state_bytes=8192) as restored:
            self.assertEqual(restored.verify(), report)

    def test_paths_and_options_are_validated_before_mutation(self):
        self.db.backup(self.backup_path)
        for call in [
            lambda: self.db.backup(bytes(self.root / "bad.backup")),
            lambda: Database.restore_backup(bytes(self.backup_path), self.destination),
            lambda: Database.restore_backup(self.backup_path, bytes(self.destination)),
            lambda: Database.restore_backup(self.backup_path, self.destination, max_frame_bytes=True),
            lambda: Database.restore_backup(self.backup_path, self.destination, max_document_bytes=0),
        ]:
            with self.assertRaises((TypeError, ValueError)):
                call()
            self.assertFalse(self.destination.exists())
        self.assertFalse((self.root / "bad.backup").exists())

    def test_backup_operations_require_open_database_and_existing_parent(self):
        missing_parent = self.root / "missing"
        with self.assertRaises(CedroError):
            self.db.backup(missing_parent / "forms.backup")
        self.assertFalse(missing_parent.exists())
        self.db.backup(self.backup_path)
        with self.assertRaises(CedroError):
            Database.restore_backup(self.backup_path, missing_parent / "restored.cedro")
        self.assertFalse(missing_parent.exists())
        self.db.close()
        for call in [self.db.verify, lambda: self.db.backup(self.root / "closed.backup")]:
            with self.assertRaises(DatabaseClosedError):
                call()
        self.assertFalse((self.root / "closed.backup").exists())


if __name__ == "__main__":
    unittest.main()
