import importlib.util, io, subprocess, sys, tarfile, tempfile, unittest
from pathlib import Path

here = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('check_npm_tarball', here / 'check-npm-tarball.py')
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)


def pack(path, names):
    with tarfile.open(path, 'w:gz') as t:
        for n, data in names.items():
            i = tarfile.TarInfo(n); i.size = len(data); t.addfile(i, io.BytesIO(data))


class Check(unittest.TestCase):
    def run_script(self, names):
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / 'p.tgz'; pack(f, names)
            return subprocess.run([sys.executable, '-I', str(here / 'check-npm-tarball.py'), str(f)], capture_output=True, text=True)

    def full(self):
        return {'package/prebuilds/%s/cedrodb.node' % k: b'x' for k in mod.expected_keys()}

    def test_complete_passes(self):
        self.assertEqual(self.run_script(self.full()).returncode, 0)

    def test_missing_platform_fails(self):
        names = self.full(); names.pop(next(iter(names)))
        r = self.run_script(names)
        self.assertNotEqual(r.returncode, 0); self.assertIn('missing native files', r.stderr)

    def test_empty_file_fails(self):
        names = self.full(); names[next(iter(names))] = b''
        self.assertNotEqual(self.run_script(names).returncode, 0)


if __name__ == '__main__':
    unittest.main()
