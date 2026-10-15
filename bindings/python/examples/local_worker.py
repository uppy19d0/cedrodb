"""Deterministic local worker: retry a task, then acknowledge it with an outbox event."""

from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database


def main() -> None:
    with TemporaryDirectory(prefix="cedrodb-local-worker-") as temporary:
        path = Path(temporary) / "worker.cedro"
        with Database(path) as db:
            queue = db.queue("reports")
            print("Enqueued:", queue.enqueue(
                "report:1", {"order_id": "order:1"}, now_ms=1000, max_attempts=3,
            ))
            first = queue.claim("worker-1", now_ms=1000, lease_ms=100)
            assert first is not None
            queue.fail(first["id"], first["lease_token"], "temporary failure", now_ms=1010, retry_delay_ms=50)
            assert queue.claim("worker-1", now_ms=1059) is None

        # There is no background worker. The application drives claim/recovery
        # and uses epoch milliseconds by default; explicit times make this demo deterministic.
        with Database(path) as db:
            queue = db.queue("reports")
            second = queue.claim("worker-2", now_ms=1060, lease_ms=100)
            assert second is not None and second["attempts"] == 2
            acknowledged = queue.ack(second["id"], second["lease_token"], now_ms=1070, operations=[
                {"kind": "put", "collection": "reports", "key": "report:1", "value": {"status": "ready"}},
                {"kind": "put", "collection": "outbox", "key": "report:1:ready",
                 "value": {"event_id": "report:1:ready", "type": "report.ready"}},
            ])
            print("Acknowledged:", acknowledged)
            print("Job:", queue.get("report:1"))
            print("Queue stats:", queue.stats(now_ms=1070))
            print("Result:", db.collection("reports").get("report:1"))
            # External services must deduplicate side effects by job/event id.
            # A lease token fences this database history, not restored forks.


if __name__ == "__main__":
    main()
