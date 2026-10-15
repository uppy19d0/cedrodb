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
    print(json.dumps({"interop": "passed", "python_to_node": True, "node_to_python": True,
                      "numeric_cas": True, "unsafe_js_integer_rejected": True}))


if __name__ == "__main__":
    main()
