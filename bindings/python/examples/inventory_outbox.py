"""Reserve inventory and append its outbox event in one conditional transaction."""

from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database


def reserve(db: Database, order_id: str, expected_available: int) -> dict:
    return db.conditional_batch([
        {"kind": "equals", "collection": "inventory", "key": "cedar-desk",
         "value": {"available": expected_available}},
        {"kind": "absent", "collection": "outbox", "key": order_id},
    ], [
        {"kind": "put", "collection": "inventory", "key": "cedar-desk",
         "value": {"available": expected_available - 1}},
        {"kind": "put", "collection": "outbox", "key": order_id,
         "value": {"event_id": order_id, "type": "inventory.reserved", "sku": "cedar-desk"}},
    ])


def main() -> None:
    with TemporaryDirectory(prefix="cedrodb-inventory-") as temporary:
        path = Path(temporary) / "shop.cedro"
        with Database(path) as db:
            db.collection("inventory").put("cedar-desk", {"available": 2})
            committed = reserve(db, "order:1", expected_available=2)
            print("Reserved:", committed)
            # A stale read cannot overwrite the inventory or append its event.
            stale = reserve(db, "order:2", expected_available=2)
            print("Stale request:", stale)
            assert committed["applied"] and not stale["applied"]
            assert committed["cursor"] == stale["cursor"]

        with Database(path) as db:
            print("Available:", db.collection("inventory").get("cedar-desk"))
            print("Atomic changes:", db.changes(since=1)["transactions"])
            # Upload outbox events with application-managed checkpoints and
            # event_id deduplication; external effects are not exactly-once.


if __name__ == "__main__":
    main()
