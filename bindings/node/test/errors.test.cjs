'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { mkdtempSync, rmSync, writeFileSync, readdirSync, readFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database, ErrorCode } = require('..');

function directory(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'cedrodb-node-errors-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test('ErrorCode is a frozen table of stable strings', () => {
  assert.ok(Object.isFrozen(ErrorCode));
  for (const [name, value] of Object.entries(ErrorCode)) assert.equal(value, `CEDRO_${name}`);
});

test('use after close throws CEDRO_CLOSED with the original message', (t) => {
  const database = new Database(directory(t));
  const collection = database.collection('a');
  database.close();
  assert.throws(() => collection.get('a'), (error) => {
    assert.equal(error.code, ErrorCode.CLOSED);
    assert.match(error.message, /closed/);
    assert.ok(error instanceof Error);
    return true;
  });
});

test('opening a locked database throws CEDRO_DATABASE_LOCKED', (t) => {
  const dir = directory(t);
  const database = new Database(dir);
  t.after(() => database.close());
  assert.throws(() => new Database(dir), (error) => {
    assert.equal(error.code, ErrorCode.DATABASE_LOCKED);
    assert.match(error.message, /already open/);
    return true;
  });
});

test('oversized documents throw CEDRO_LIMIT_EXCEEDED', (t) => {
  const database = new Database(directory(t), { maxDocumentBytes: 30 });
  t.after(() => database.close());
  assert.throws(() => database.collection('a').put('k', { text: 'x'.repeat(100) }), (error) => {
    assert.equal(error.code, ErrorCode.LIMIT_EXCEEDED);
    assert.match(error.message, /limit exceeded/);
    return true;
  });
});

test('engine-rejected input throws CEDRO_INVALID_INPUT', (t) => {
  const database = new Database(directory(t));
  t.after(() => database.close());
  assert.throws(() => database.collection('_cedro.jobs.x').put('k', 1), (error) => {
    assert.equal(error.code, ErrorCode.INVALID_INPUT);
    return true;
  });
});

test('an unreadable database path throws CEDRO_IO', (t) => {
  const dir = directory(t);
  const file = path.join(dir, 'plain-file');
  writeFileSync(file, 'x');
  assert.throws(() => new Database(path.join(file, 'child')), (error) => {
    assert.equal(error.code, ErrorCode.IO);
    return true;
  });
});

test('a garbage log throws CEDRO_CORRUPTION', (t) => {
  const dir = directory(t);
  const database = new Database(dir);
  database.collection('a').put('k', 1);
  database.close();
  const target = readdirSync(dir).find((name) => name.endsWith('.log') || name.endsWith('.cedro')) ;
  assert.ok(target, `no log file found in ${readdirSync(dir)}`);
  const file = path.join(dir, target);
  const bytes = readFileSync(file);
  bytes[0] ^= 0xff;
  writeFileSync(file, bytes);
  assert.throws(() => new Database(dir), (error) => {
    assert.equal(error.code, ErrorCode.CORRUPTION);
    assert.match(error.message, /corrupt/);
    return true;
  });
});
