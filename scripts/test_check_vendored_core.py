import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "check_vendored_core", Path(__file__).with_name("check-vendored-core.py"))
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
TARGETS = {"copy": ["Cargo.toml", "LICENSE"]}


def build(root: Path):
    (root / "src").mkdir()
    (root / "copy" / "src").mkdir(parents=True)
    for rel in ["Cargo.toml", "LICENSE", "src/lib.rs"]:
        (root / rel).write_text(rel)
        (root / "copy" / rel).write_text(rel)


class CheckVendoredCore(unittest.TestCase):
    def check(self, mutate):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build(root)
            mutate(root)
            return mod.problems(root, TARGETS)

    def test_identical_copy_passes(self):
        self.assertEqual(self.check(lambda r: None), [])

    def test_changed_file_is_reported(self):
        out = self.check(lambda r: (r / "copy/src/lib.rs").write_text("old"))
        self.assertEqual(len(out), 1)
        self.assertIn("lib.rs", out[0])

    def test_missing_file_is_reported(self):
        out = self.check(lambda r: (r / "copy/src/lib.rs").unlink())
        self.assertIn("missing from the copy", out[0])

    def test_extra_file_is_reported(self):
        out = self.check(lambda r: (r / "copy/src/old.rs").write_text("x"))
        self.assertIn("not present in the engine", out[0])

    def test_missing_copy_is_reported(self):
        def drop(r):
            import shutil
            shutil.rmtree(r / "copy")
        self.assertIn("missing", self.check(drop)[0])


if __name__ == "__main__":
    unittest.main()
