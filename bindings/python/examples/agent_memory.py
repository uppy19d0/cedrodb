"""Offline checkpoint/outbox and retrieval example; no model or API key required.

The tiny vectors below are synthetic ranking fixtures, NOT model embeddings.
For an actual application, provide embeddings from your chosen local or remote
model and store its model/version alongside your documents.
"""

from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database


def main():
    with TemporaryDirectory(prefix="cedrodb-agent-example-") as directory:
        path = Path(directory) / "assistant.cedro"
        with Database(path) as db:
            knowledge = db.collection("knowledge")
            knowledge.put_many({
                "local": {"owner": "ana", "text": "local backup recovery", "embedding": [1, 0]},
                "queue": {"owner": "ana", "text": "durable worker retries", "embedding": [0.6, 0.8]},
                "other": {"owner": "luis", "text": "private notes", "embedding": [0, 1]},
            })
            knowledge.create_index("owner")
            matches = knowledge.search(
                text="backup", vector=[1, 0], limit=2,
                filters=[{"field": "owner", "op": "eq", "value": "ana"}],
            )
            assert matches["mode"] == "hybrid"
            assert matches["hits"][0]["key"] == "local"
            agent = db.agent("support")
            created = agent.checkpoint(
                "conversation:1", {"question": "How do I recover a backup?"},
                metadata={"owner": "ana", "state_format": "support.v1"}, now_ms=1000,
            )
            revision = created["checkpoint"]["revision"]
            completed = agent.checkpoint(
                "conversation:1", {"answer": "Use restore_backup with a new destination."},
                expected_revision=revision, status="completed", now_ms=1001,
                operations=[{
                    "kind": "put", "collection": "outbox", "key": "conversation:1:answered",
                    "value": {"event_id": "conversation:1:answered", "type": "answer.ready"},
                }],
            )
            assert completed["applied"]
            stale = agent.checkpoint("conversation:1", {}, expected_revision=revision, now_ms=1002)
            assert not stale["applied"]
            cursor = completed["cursor"]
        with Database(path) as reopened:
            checkpoint = reopened.agent("support").get("conversation:1")
            assert checkpoint["revision"] == cursor
            assert checkpoint["status"] == "completed"
            assert reopened.collection("outbox").get("conversation:1:answered") is not None
            print({"revision": cursor, "stale_rejected": not stale["applied"],
                   "recovered": checkpoint["state"],
                   "ranked_keys": [hit["key"] for hit in matches["hits"]]})


if __name__ == "__main__":
    main()
