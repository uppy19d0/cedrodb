#!/usr/bin/env python3
"""Keep core and native SDK package versions aligned before a release."""
import argparse
import json
import pathlib
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('version')
args = parser.parse_args()
if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', args.version):
    parser.error('use a stable three-part package version')
root = pathlib.Path(__file__).resolve().parents[1]
for name in ('Cargo.toml', 'bindings/node/Cargo.toml', 'bindings/python/Cargo.toml', 'bindings/python/pyproject.toml'):
    path = root / name
    text = re.sub(r'^version = "[^"]+"', 'version = "' + args.version + '"', path.read_text(), count=1, flags=re.M)
    text = re.sub(r'(cedrodb = \{ path = "core", version = ")=?[^\"]+', r'\g<1>=' + args.version, text)
    path.write_text(text)
for name in ('Cargo.lock', 'bindings/node/Cargo.lock', 'bindings/python/Cargo.lock'):
    path = root / name
    path.write_text(re.sub(r'(name = "cedrodb(?:-node|-python)?"\nversion = ")[^"]+', r'\g<1>' + args.version, path.read_text()))
path = root / 'bindings/node/package.json'
package = json.loads(path.read_text())
package['version'] = args.version
path.write_text(json.dumps(package, indent=2) + '\n')
for name in ('bindings/node/index.cjs', 'bindings/node/index.d.ts', 'bindings/node/test/types.cts'):
    path = root / name
    path.write_text(re.sub(r"(version:\s*)'\d+\.\d+\.\d+'", r"\g<1>'" + args.version + "'", path.read_text()))
path = root / 'bindings/python/python/cedrodb/__init__.py'
path.write_text(re.sub(r'__version__ = "[^"]+"', '__version__ = "' + args.version + '"', path.read_text()))
print('Core, native SDKs and lockfiles aligned at ' + args.version)
