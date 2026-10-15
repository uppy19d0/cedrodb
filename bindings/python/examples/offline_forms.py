"""Save offline forms plus events, then verify, back up and restore the store."""

from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database


def main() -> None:
    with TemporaryDirectory(prefix="cedrodb-offline-forms-") as temporary:
        directory = Path(temporary)
        source = directory / "field-app.cedro"
        backup = directory / "field-app.backup"
        restored = directory / "recovered.cedro"

        with Database(source) as db:
            with db.transaction() as tx:
                tx.put("forms", "inspection:1", {
                    "site": "Santo Domingo", "status": "pending_upload",
                    "answers": {"equipment_ok": True, "temperature": 24.5},
                })
                tx.put("outbox", "inspection:1:submitted", {
                    "event_id": "inspection:1:submitted",
                    "type": "inspection.submitted", "form_id": "inspection:1",
                })
            print("Integrity:", db.verify())
            print("Backup:", db.backup(backup))

        # Restore validates the entire backup before creating a new destination.
        # Both backup file and destination directory must not already exist.
        print("Restored:", Database.restore_backup(backup, restored))
        with Database(restored) as db:
            forms = db.collection("forms")
            forms.create_index("status")
            print("Pending forms:", forms.find([
                {"field": "status", "op": "eq", "value": "pending_upload"}
            ]))
            print("Recovered events:", db.changes(since=0)["transactions"])
            # A real uploader handles outbox events and persists its own cursor
            # after successful delivery, deduplicating retries by event_id.


if __name__ == "__main__":
    main()
