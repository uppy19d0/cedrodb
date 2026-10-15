"""Copy the allowlisted Rust core into this independently buildable package.

Run before building a checkout. Published source distributions already contain
the core and never need files outside this package. This copies source only;
database files, secrets, build outputs and sibling SDKs are never included.
"""

from pathlib import Path
import shutil


def prepare() -> None:
    package = Path(__file__).resolve().parents[1]
    source = package.parents[1]
    destination = package / "core"
    manifest = source / "Cargo.toml"
    if not manifest.is_file():
        if (destination / "Cargo.toml").is_file():
            print("Using the Rust core included in this source distribution.")
            return
        raise SystemExit("Run this script from a CedroDB checkout or an intact sdist.")
    if 'name = "cedrodb"' not in manifest.read_text(encoding="utf-8"):
        raise SystemExit("Refusing to copy an unrecognized parent crate.")
    files = [manifest, source / "LICENSE", *sorted((source / "src").rglob("*.rs"))]
    if any(item.is_symlink() or not item.is_file() for item in files):
        raise SystemExit("Core source must contain regular files, not symbolic links.")
    if destination.exists():
        if destination.is_symlink():
            raise SystemExit("Refusing to replace a symlink at the vendored core path.")
        shutil.rmtree(destination)
    for item in files:
        output = destination / item.relative_to(source)
        output.parent.mkdir(parents=True, exist_ok=True)
        # Fresh mtimes ensure Cargo notices a newly prepared source snapshot,
        # even if the engine files came from an older checkout/archive.
        shutil.copyfile(item, output)
    shutil.copy2(source / "LICENSE", package / "LICENSE")
    print("Prepared {} allowlisted core files.".format(len(files)))


if __name__ == "__main__":
    prepare()
