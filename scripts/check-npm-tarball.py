#!/usr/bin/env python3
"""Fail when a packed npm tarball lacks a native engine for any supported platform.

Usage: check-npm-tarball.py PACKAGE.tgz
The expected platform keys come from bindings/node/platform.cjs (supportedPlatforms).
"""
import json
from pathlib import Path
import subprocess
import sys
import tarfile

root = Path(__file__).resolve().parents[1]


def expected_keys():
    out = subprocess.run(
        ['node', '-e', "console.log(JSON.stringify(require('./platform.cjs').supportedPlatforms))"],
        cwd=root / 'bindings' / 'node', check=True, capture_output=True, text=True,
    ).stdout
    keys = json.loads(out)
    assert keys and all(isinstance(k, str) and k for k in keys), 'no supported platforms declared'
    return keys


def missing_natives(names, keys):
    return [k for k in keys if 'package/prebuilds/%s/cedrodb.node' % k not in names]


def main(argv):
    if len(argv) != 2:
        sys.exit('usage: check-npm-tarball.py PACKAGE.tgz')
    with tarfile.open(argv[1]) as tar:
        members = {m.name: m for m in tar.getmembers() if m.isfile()}
    keys = expected_keys()
    missing = missing_natives(members, keys)
    empty = [k for k in keys if k not in missing and members['package/prebuilds/%s/cedrodb.node' % k].size == 0]
    if missing or empty:
        sys.exit('npm tarball %s: missing native files for %s; empty for %s' % (argv[1], missing, empty))
    print('npm tarball contains native engines for: ' + ', '.join(keys))


if __name__ == '__main__':
    main(sys.argv)
