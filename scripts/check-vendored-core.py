"""Fail if the vendored engine copies differ from the engine sources.

bindings/node/core and bindings/python/core are generated (git-ignored) by
bindings/node/scripts/vendor-core.mjs and bindings/python/scripts/prepare_core.py.
A stale copy silently builds an older engine into a binding. This compares the
files each vendoring script copies against the repository root.

Usage: python scripts/check-vendored-core.py
"""

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
# (copy directory, extra top-level files that script vendors besides src/*.rs)
TARGETS = {
    "bindings/node/core": ["Cargo.toml", "Cargo.lock", "LICENSE"],
    "bindings/python/core": ["Cargo.toml", "LICENSE"],
}


def problems(root: Path = ROOT, targets=TARGETS):
    found = []
    for name, extras in targets.items():
        copy = root / name
        if not copy.is_dir():
            found.append(f"{name}: missing; run the vendoring script")
            continue
        expected = {f"src/{p.relative_to(root / 'src').as_posix()}"
                    for p in (root / "src").rglob("*.rs")} | set(extras)
        actual = {p.relative_to(copy).as_posix() for p in (copy / "src").rglob("*.rs")}
        actual |= {e for e in extras if (copy / e).is_file()}
        for rel in sorted(expected | actual):
            source, vendored = root / rel, copy / rel
            if rel not in actual:
                found.append(f"{name}/{rel}: missing from the copy")
            elif rel not in expected:
                found.append(f"{name}/{rel}: not present in the engine")
            elif source.read_bytes() != vendored.read_bytes():
                found.append(f"{name}/{rel}: differs from {rel}")
    return found


def main() -> int:
    found = problems()
    for line in found:
        print(line, file=sys.stderr)
    if found:
        print("Vendored engine copies are stale. Regenerate with `npm run build` "
              "in bindings/node and `python scripts/prepare_core.py` in bindings/python.",
              file=sys.stderr)
        return 1
    print("Vendored engine copies match src/.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
