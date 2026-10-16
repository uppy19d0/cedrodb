"""Rebuild and test an sdist without access to its original repository.

Usage: python scripts/verify_sdist.py dist/cedrodb-0.2.0.tar.gz
Run in a virtual environment containing maturin. Rust dependencies must be
available in the Cargo cache or downloadable; Python build isolation is disabled
so this check can also run offline with already installed packaging tools.
"""

from pathlib import Path, PurePosixPath
import os
import subprocess
import sys
import tarfile
from tempfile import TemporaryDirectory


def verify(artifact: Path) -> None:
    build_environment = os.environ.copy()
    build_environment["PATH"] = str(Path(sys.executable).parent) + os.pathsep + build_environment.get("PATH", "")
    with TemporaryDirectory(prefix="cedrodb-isolated-sdist-") as temporary:
        directory = Path(temporary)
        with tarfile.open(artifact.resolve(), "r:gz") as archive:
            members = archive.getmembers()
            for member in members:
                name = PurePosixPath(member.name)
                if name.is_absolute() or ".." in name.parts or not (member.isfile() or member.isdir()):
                    raise SystemExit("Unexpected unsafe member in source archive")
            archive.extractall(directory)
        candidates = list(directory.glob("*/pyproject.toml"))
        if len(candidates) != 1:
            raise SystemExit("Expected exactly one source project")
        source = candidates[0].parent
        for relative in ["core/Cargo.toml", "core/LICENSE", "core/src/lib.rs", "tests/test_database.py"]:
            if not (source / relative).is_file():
                raise SystemExit("Missing bundled source: " + relative)
        wheels = directory / "wheels"
        subprocess.run([
            sys.executable, "-m", "pip", "wheel", "--no-build-isolation", "--no-deps",
            "--no-cache-dir", str(source), "--wheel-dir", str(wheels),
        ], cwd=directory, env=build_environment, check=True)
        wheel = next(wheels.glob("*.whl"))
        installed = directory / "installed"
        subprocess.run([
            sys.executable, "-m", "pip", "install", "--no-deps", "--no-cache-dir",
            "--target", str(installed), str(wheel),
        ], cwd=directory, env=build_environment, check=True)
        test = "\n".join([
            "import pathlib, sys, unittest, importlib.metadata",
            "sys.path.insert(0, sys.argv[1])",
            "import cedrodb._native as native",
            "entry = next(e for e in importlib.metadata.distribution('cedrodb').entry_points if e.group == 'console_scripts' and e.name == 'cedrodb')",
            "assert entry.value == 'cedrodb.cli:main' and callable(entry.load())",
            "assert pathlib.Path(native.__file__).resolve().is_relative_to(pathlib.Path(sys.argv[1]).resolve())",
            "suite = unittest.defaultTestLoader.discover(sys.argv[2])",
            "result = unittest.TextTestRunner(verbosity=2).run(suite)",
            "raise SystemExit(not result.wasSuccessful())",
        ])
        test_environment = build_environment.copy()
        # CLI tests spawn fresh interpreters. Give them the same isolated wheel
        # as the parent instead of an unrelated package in the build venv.
        test_environment["PYTHONPATH"] = str(installed)
        test_environment["PYTHONNOUSERSITE"] = "1"
        subprocess.run([
            sys.executable, "-s", "-c", test, str(installed), str(source / "tests"),
        ], cwd=directory, env=test_environment, check=True)
        print("Self-contained sdist rebuilt, installed and tested successfully.")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python scripts/verify_sdist.py dist/cedrodb-0.2.0.tar.gz")
    verify(Path(sys.argv[1]))
