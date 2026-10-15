#!/usr/bin/env python3
"""Fail CI when core, SDKs, declarations or install instructions disagree."""
import json
from pathlib import Path
import re

root = Path(__file__).resolve().parents[1]
def read(name):
    return (root / name).read_text()
def version(name):
    match = re.search(r'^version = "([^"]+)"', read(name), re.M)
    if not match:
        raise ValueError('missing package version in ' + name)
    return match[1]

expected = version('Cargo.toml')
for name in ('bindings/node/Cargo.toml', 'bindings/python/Cargo.toml', 'bindings/python/pyproject.toml'):
    assert version(name) == expected, name
for name in ('bindings/node/Cargo.toml', 'bindings/python/Cargo.toml'):
    assert re.search(r'cedrodb = \{ path = "core", version = "=?' + re.escape(expected) + r'"', read(name)), name
for name in ('Cargo.lock', 'bindings/node/Cargo.lock', 'bindings/python/Cargo.lock'):
    found = re.findall(r'name = "(cedrodb(?:-node|-python)?)"\nversion = "([^"]+)"', read(name))
    assert found and all(value == expected for _, value in found), name
assert json.loads(read('bindings/node/package.json'))['version'] == expected
assert "version: '" + expected + "'" in read('bindings/node/index.cjs')
assert "export const version: '" + expected + "'" in read('bindings/node/index.d.ts')
assert '__version__ = "' + expected + '"' in read('bindings/python/python/cedrodb/__init__.py')
assert 'npm install cedrodb@' + expected in read('README.md')
assert 'npm install cedrodb@' + expected in read('bindings/node/README.md')
assert '**Experimental ' + expected.rsplit('.', 1)[0] + ' release.**' in read('bindings/node/README.md')
assert 'pip install cedrodb==' + expected in read('bindings/python/README.md')
assert '## ' + expected in read('CHANGELOG.md')
print('Release metadata and public install instructions agree at ' + expected)
