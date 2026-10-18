'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const env = { ...process.env, CEDRODB_TEST_PLATFORM_KEY: 'fakeos-fakearch' };
const { supportedPlatforms } = require('../platform.cjs');

test('native.cjs names the platform key and supported platforms on unsupported platform', () => {
  const r = spawnSync(process.execPath, ['-e', "require('./native.cjs')"], { cwd: root, env, encoding: 'utf8' });
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /platform fakeos-fakearch/);
  for (const key of supportedPlatforms) assert.ok(r.stderr.includes(key), key);
});

test('build.mjs --if-needed warns and exits 0 when cargo is unavailable', () => {
  const r = spawnSync(process.execPath, ['scripts/build.mjs', '--if-needed'], {
    cwd: root, env: { ...env, PATH: '/nonexistent' }, encoding: 'utf8',
  });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stderr, /fakeos-fakearch/);
  assert.match(r.stderr, /Supported prebuilt platforms/);
  assert.match(r.stderr, /npm rebuild cedrodb/);
});

test('build.mjs without --if-needed still fails when cargo is unavailable', () => {
  const r = spawnSync(process.execPath, ['scripts/build.mjs'], {
    cwd: root, env: { ...env, PATH: '/nonexistent' }, encoding: 'utf8',
  });
  assert.notEqual(r.status, 0);
});
