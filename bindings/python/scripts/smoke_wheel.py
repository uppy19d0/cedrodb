"""Smoke test an installed cedrodb wheel: open, commit, close, reopen, read.

Run from a clean virtualenv with only the wheel installed (no Rust toolchain,
no source checkout on sys.path). Exits non-zero on any failure.
"""

import sys
import tempfile
from pathlib import Path

import cedrodb
from cedrodb import Database, _native


def main():
    native = str(_native.__file__)
    if not native.endswith((".so", ".pyd")):
        raise SystemExit("native extension not loaded from a compiled module: " + native)
    with tempfile.TemporaryDirectory(prefix="cedrodb-smoke-") as directory:
        path = Path(directory) / "smoke.cedro"
        with Database(path) as db:
            orders = db.collection("orders")
            orders.put("a", {"total": 12.5, "tag": "café"})
            with db.transaction() as tx:
                tx.put("orders", "b", [1, {"nested": True}])
        with Database(path) as db:
            orders = db.collection("orders")
            if orders.get("a") != {"total": 12.5, "tag": "café"}:
                raise SystemExit("reopen returned wrong value for a")
            if orders.get("b") != [1, {"nested": True}]:
                raise SystemExit("reopen returned wrong value for b")
    print("cedrodb smoke ok: python %s, native %s" % (sys.version.split()[0], native))


if __name__ == "__main__":
    main()
