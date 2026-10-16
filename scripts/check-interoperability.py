#!/usr/bin/env python3
"""Exercise one real database from Python and Node, sequentially releasing locks."""

import json
import pathlib
import subprocess
import tempfile

from cedrodb import Database


def main():
    root = pathlib.Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="cedrodb-interop-") as directory:
        with Database(directory) as database:
            database.collection("records").put("python", {"ratio": 1.0, "source": "python"})
        subprocess.run(["node", str(root / "scripts/interop-node.mjs"), directory, "exchange"], check=True)
        with Database(directory) as database:
            records = database.collection("records")
            assert records.get("python") == {"ratio": 1, "source": "node"}
            assert records.get("node") == {"source": "node", "count": 2}
            changes = database.changes(since=0, limit=100)
            assert changes["next_cursor"] == 3
            assert len(changes["transactions"]) == 3
            database.collection("large").put("integer", {"value": 2**64 - 1})
            assert database.collection("large").get("integer")["value"] == 2**64 - 1
        subprocess.run(["node", str(root / "scripts/interop-node.mjs"), directory, "precision"], check=True)
        exact = 0.19450526354013234
        with Database(directory) as database:
            database.collection("memory").put("policy", {"text": "refund policy", "embedding": [exact, 1.0], "model": "demo"})
            database.collection("memory").put("other", {"embedding": [0, 0, 0], "model": "other"})
            saved = database.agent("interop").checkpoint(
                "thread:1", {"ratio": exact, "source": "python"},
                metadata={"ratio": exact}, now_ms=1000,
                operations=[{"kind": "put", "collection": "outbox", "key": "python-agent", "value": {"ready": True}}],
            )
            assert saved["applied"]
            revision = saved["checkpoint"]["revision"]
        subprocess.run(["node", str(root / "scripts/interop-node.mjs"), directory, "agent"], check=True)
        with Database(directory) as database:
            agents = database.agent("interop")
            current = agents.get("thread:1")
            assert current["state"] == {"ratio": exact, "source": "node"}
            assert current["metadata"] == {"ratio": exact}
            assert current["revision"] > revision
            conflict = agents.checkpoint("thread:1", {"source": "stale"}, expected_revision=revision,
                operations=[{"kind":"put", "collection":"outbox", "key":"must-not-exist", "value":{}}])
            assert not conflict["applied"] and conflict["checkpoint"] == current
            assert database.collection("outbox").get("must-not-exist") is None
            assert database.collection("results").get("node-agent") == {"ratio": exact}
            database.collection("memory").create_index("model")
            hits = database.collection("memory").search(text="refund", vector=[exact, 1.0], min_vector_score=0.99,
                filters=[{"field":"model", "op":"eq", "value":"demo"}])
            assert hits["scanned"] == 1 and [hit["key"] for hit in hits["hits"]] == ["policy"]
    print(json.dumps({"interop": "passed", "python_to_node": True, "node_to_python": True,
                      "numeric_cas": True, "unsafe_js_integer_rejected": True,
                      "agent_revision_fencing": True, "atomic_agent_results": True,
                      "hybrid_memory": True, "exact_float_roundtrip": True}))


if __name__ == "__main__":
    main()
