'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database } = require('..');

function fixture(t) {
  const root = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-backup-'));
  const directory = path.join(root, 'database');
  const database = new Database(directory);
  t.after(() => { database.close(); fs.rmSync(root, { recursive: true, force: true }); });
  return { database, directory, root };
}

test('verified backups restore a committed point in time with its original change cursors', (t) => {
  const { database, root } = fixture(t);
  const reports = database.collection('reports');
  database.batch([
    { kind: 'put', collection: 'reports', key: '001', value: { state: 'pending', text: 'Inspección 🌲' } },
    { kind: 'put', collection: 'checkpoints', key: 'sync', value: 0 },
  ]);
  reports.put('002', null);
  reports.createIndex('state');
  const originalChanges = database.changes();
  const stats = database.stats();
  const verified = database.verify();
  assert.deepEqual(verified, {
    logBytes: stats.logBytes, transactions: stats.latestCursor,
    records: stats.records, contentBytes: stats.contentBytes,
  });
  const backup = path.join(root, 'offline.cedro-backup');
  assert.deepEqual(database.backup(backup), verified);
  assert.equal(database.stats().latestCursor, stats.latestCursor);
  reports.put('001', { state: 'sent' }); // Later changes do not alter the backup.

  const restoredDirectory = path.join(root, 'restored');
  assert.deepEqual(Database.restoreBackup(backup, restoredDirectory), verified);
  const restored = new Database(restoredDirectory);
  try {
    assert.deepEqual(restored.collection('reports').get('001'), { state: 'pending', text: 'Inspección 🌲' });
    assert.equal(restored.collection('reports').get('002'), null);
    assert.deepEqual(restored.changes(), originalChanges);
    assert.deepEqual(restored.verify(), verified);
    assert.equal(restored.stats().indexes, 0);
    restored.collection('reports').put('003', { state: 'pending' });
    assert.equal(restored.changes({ since: verified.transactions }).transactions[0].cursor, verified.transactions + 1);
  } finally { restored.close(); }
});

test('backup and restore reject existing targets without overwriting their contents', (t) => {
  const { database, root } = fixture(t);
  database.collection('c').put('a', { n: 1 });
  const occupiedFile = path.join(root, 'occupied.backup');
  fs.writeFileSync(occupiedFile, 'keep me');
  assert.throws(() => database.backup(occupiedFile));
  assert.equal(fs.readFileSync(occupiedFile, 'utf8'), 'keep me');

  const backup = path.join(root, 'valid.backup');
  database.backup(backup);
  const occupiedDirectory = path.join(root, 'occupied');
  fs.mkdirSync(occupiedDirectory);
  assert.throws(() => Database.restoreBackup(backup, occupiedDirectory));
  assert.deepEqual(fs.readdirSync(occupiedDirectory), []);
  fs.writeFileSync(path.join(occupiedDirectory, 'keep'), 'original');
  assert.throws(() => Database.restoreBackup(backup, occupiedDirectory));
  assert.equal(fs.readFileSync(path.join(occupiedDirectory, 'keep'), 'utf8'), 'original');
});

test('restore rejects corrupt, truncated and raw-log inputs before creating its target', (t) => {
  const { database, directory, root } = fixture(t);
  database.collection('c').put('a', { payload: 'original' });
  const backup = path.join(root, 'original.backup');
  database.backup(backup);
  const contents = fs.readFileSync(backup);
  const corrupt = Buffer.from(contents);
  corrupt[corrupt.length - 1] ^= 0x01;
  const cases = [corrupt, contents.subarray(0, contents.length - 1), Buffer.concat([contents, Buffer.from([0])]), fs.readFileSync(path.join(directory, 'cedro.log'))];
  for (const [index, bytes] of cases.entries()) {
    const input = path.join(root, `invalid-${index}.backup`);
    const target = path.join(root, `invalid-target-${index}`);
    fs.writeFileSync(input, bytes);
    assert.throws(() => Database.restoreBackup(input, target));
    assert.equal(fs.existsSync(target), false);
  }
});

test('restore enforces configured limits and requires existing parent directories', (t) => {
  const { database, root } = fixture(t);
  database.collection('c').put('a', { payload: 'long enough to exceed one byte' });
  const backup = path.join(root, 'limits.backup');
  database.backup(backup);
  const limited = path.join(root, 'limited');
  assert.throws(() => Database.restoreBackup(backup, limited, { maxStateBytes: 1 }), /limit/i);
  assert.equal(fs.existsSync(limited), false);
  const absentParent = path.join(root, 'absent');
  assert.throws(() => database.backup(path.join(absentParent, 'data.backup')));
  assert.throws(() => Database.restoreBackup(backup, path.join(absentParent, 'data')));
  assert.equal(fs.existsSync(absentParent), false);
  const restoredDirectory = path.join(root, 'within-limits');
  assert.equal(Database.restoreBackup(backup, restoredDirectory, { maxStateBytes: 1024 }).records, 1);
});

test('strict verification detects an incomplete tail without repairing the live log', (t) => {
  const { database, directory, root } = fixture(t);
  database.collection('c').put('a', true);
  const log = path.join(directory, 'cedro.log');
  // Controlled fault injection: applications must never mutate an open log.
  fs.appendFileSync(log, Buffer.from([0x12, 0x34, 0x56]));
  const damaged = fs.readFileSync(log);
  assert.throws(() => database.verify());
  assert.deepEqual(fs.readFileSync(log), damaged);
  const backup = path.join(root, 'damaged.backup');
  assert.throws(() => database.backup(backup));
  assert.equal(fs.existsSync(backup), false);
});

test('backup APIs validate paths and options and reject operations after closing', (t) => {
  const { database, root } = fixture(t);
  assert.throws(() => database.backup(''), TypeError);
  assert.throws(() => Database.restoreBackup('x', '', {}), TypeError);
  assert.throws(() => Database.restoreBackup('x', path.join(root, 'target'), { maxStateBytes: 0 }), RangeError);
  assert.throws(() => Database.restoreBackup('x', path.join(root, 'target'), { typo: 10 }), TypeError);
  database.close();
  assert.throws(() => database.verify(), /closed/);
  assert.throws(() => database.backup(path.join(root, 'closed.backup')), /closed/);
});

