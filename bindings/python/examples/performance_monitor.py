"""Sample diagnostics from the live owner handle; no second database process.

Run from an installed CedroDB >=0.7. Metrics exclude document values, but
collection and index names are visible. Apply your own logging access policy.
"""
import json
import threading
from pathlib import Path
from tempfile import TemporaryDirectory

from cedrodb import Database


def monitor(db, stop):
    while not stop.wait(0.25):
        print(json.dumps({"kind": "cedro.diagnostics", "snapshot": db.diagnostics()}))


def main():
    with TemporaryDirectory(prefix="cedrodb-monitor-") as directory:
        with Database(Path(directory) / "example") as db:
            stop = threading.Event()
            sampler = threading.Thread(target=monitor, args=(db, stop), daemon=True)
            sampler.start()
            try:
                memory = db.collection("memory")
                memory.put_many({
                    str(i): {"tenant": str(i % 10), "text": "local memory"}
                    for i in range(1000)
                })
                predicate = [{"field": "tenant", "op": "eq", "value": "0"}]
                scan = memory.profile_query(predicate, limit=1000)
                memory.create_index("tenant")
                indexed = memory.profile_query(predicate, limit=1000)
                assert scan["page"]["records"] == indexed["page"]["records"]
                print(json.dumps({"scan": scan["plan"], "indexed": indexed["plan"]}))
                # Keep the sample application alive briefly, rather than claiming
                # a background service is included with the database engine.
                stop.wait(0.6)
            finally:
                stop.set()
                sampler.join()
            print(json.dumps({"kind": "cedro.final", "snapshot": db.diagnostics()}))


if __name__ == "__main__":
    main()
