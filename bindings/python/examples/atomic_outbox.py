"""Run with an installed CedroDB wheel: python examples/atomic_outbox.py."""

from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database, MISSING


def main() -> None:
    with TemporaryDirectory(prefix="cedrodb-outbox-") as directory:
        path = Path(directory) / "shop.cedro"
        with Database(path) as db:
            with db.transaction() as tx:
                tx.put("orders", "order:1", {"status": "paid", "total": 120})
                tx.put("outbox", "event:1", {
                    "event_id": "event:1", "type": "order.paid", "order_id": "order:1"
                })
            orders = db.collection("orders")
            orders.create_index("status")
            print("Paid orders:", orders.find([{"field": "status", "op": "eq", "value": "paid"}]))

        # The feed comes from the durable transaction log, including after reopen.
        with Database(path) as db:
            page = db.changes(since=0)
            for transaction in page["transactions"]:
                for operation in transaction["operations"]:
                    if operation["collection"] != "outbox" or operation["kind"] != "put":
                        continue
                    event = operation["value"]
                    # Real external deliveries should deduplicate by event_id.
                    print("Deliver event:", event)
            checkpoints = db.collection("checkpoints")
            saved = checkpoints.compare_exchange(
                "example-consumer", expected=MISSING,
                replacement={"cursor": page["next_cursor"]},
            )
            print("Checkpoint saved:", saved)
            print("Stats:", db.stats())


if __name__ == "__main__":
    main()
