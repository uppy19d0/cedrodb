'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database, Queue } = require('..');

function fixture(t, options) {
  const root = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-queue-'));
  const directory = path.join(root, 'database');
  const database = new Database(directory, options);
  t.after(() => { database.close(); fs.rmSync(root, { recursive: true, force: true }); });
  return { root, directory, database, queue: database.queue('reports') };
}

test('enqueue returns camelCase fields and deduplicates stable IDs', (t) => {
  const { database, queue } = fixture(t);
  assert.ok(queue instanceof Queue);
  assert.equal(queue.name, 'reports');
  assert.equal(queue.get('missing'), undefined);
  const first = queue.enqueue('report:1', { title: 'Cedro 🌲' }, { nowMs: 1000 });
  assert.deepEqual(first, { inserted: true, job: {
    schemaVersion: 1, id: 'report:1', payload: { title: 'Cedro 🌲' }, status: 'queued',
    attempts: 0, maxAttempts: 3, priority: 0, createdAt: 1000, availableAt: 1000,
    leaseUntil: null, leaseToken: null, worker: null, lastError: null,
  } });
  const cursor = database.stats().latestCursor;
  assert.deepEqual(queue.enqueue('report:1', { ignored: true }, {
    nowMs: 2000, delayMs: 500, maxAttempts: 1, priority: 100,
  }), { inserted: false, job: first.job });
  assert.equal(database.stats().latestCursor, cursor);
  first.job.payload.title = 'changed by caller';
  assert.deepEqual(queue.get('report:1').payload, { title: 'Cedro 🌲' });
});

test('claim orders ready jobs by priority, availability and stable ID', (t) => {
  const { queue } = fixture(t);
  queue.enqueue('low', null, { nowMs: 1000, priority: 1 });
  queue.enqueue('future', null, { nowMs: 1000, priority: 10, delayMs: 100 });
  queue.enqueue('medium-b', null, { nowMs: 1000, priority: 5 });
  queue.enqueue('medium-a', null, { nowMs: 1000, priority: 5 });
  assert.deepEqual(queue.stats({ nowMs: 1000 }), { queued: 4, leased: 0, done: 0, dead: 0, ready: 3, expired: 0 });
  assert.equal(queue.claim('worker', { nowMs: 1000, leaseMs: 1000 }).id, 'medium-a');
  assert.equal(queue.claim('worker', { nowMs: 1000, leaseMs: 1000 }).id, 'medium-b');
  assert.equal(queue.claim('worker', { nowMs: 1000, leaseMs: 1000 }).id, 'low');
  assert.equal(queue.claim('worker', { nowMs: 1099, leaseMs: 1000 }), undefined);
  assert.equal(queue.claim('worker', { nowMs: 1100, leaseMs: 1000 }).id, 'future');
});

test('expired leases are fenced and reclaim invalidates every old-token operation', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', true, { nowMs: 1000 });
  const first = queue.claim('worker:1', { nowMs: 1000, leaseMs: 100 });
  assert.equal(first.attempts, 1);
  assert.equal(first.status, 'leased');
  assert.equal(first.worker, 'worker:1');
  assert.equal(first.leaseUntil, 1100);
  assert.equal(queue.ack('job', first.leaseToken, { nowMs: 1100 }), false);
  const second = queue.claim('worker:2', { nowMs: 1100, leaseMs: 100 });
  assert.equal(second.attempts, 2);
  assert.notEqual(second.leaseToken, first.leaseToken);
  const cursor = database.stats().latestCursor;
  assert.equal(queue.ack('job', first.leaseToken, {
    nowMs: 1101, operations: [{ kind: 'put', collection: 'results', key: 'stale', value: true }],
  }), false);
  assert.equal(database.collection('results').get('stale'), undefined);
  assert.equal(queue.fail('job', first.leaseToken, 'late failure', { nowMs: 1101 }), false);
  assert.equal(queue.extend('job', first.leaseToken, { nowMs: 1101, leaseMs: 100 }), false);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(queue.ack('job', second.leaseToken, { nowMs: 1101 }), true);
  assert.equal(queue.get('job').status, 'done');
});

test('fail schedules retries, exhausts attempts and permits an explicit dead-job retry', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', { report: 42 }, { nowMs: 1000, maxAttempts: 2 });
  const first = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  assert.equal(queue.fail('job', first.leaseToken, 'renderer unavailable', { nowMs: 1010, retryDelayMs: 50 }), true);
  const waiting = queue.get('job');
  assert.equal(waiting.status, 'queued');
  assert.equal(waiting.attempts, 1);
  assert.equal(waiting.availableAt, 1060);
  assert.equal(waiting.lastError, 'renderer unavailable');
  assert.equal(queue.claim('worker', { nowMs: 1059 }), undefined);
  const second = queue.claim('worker', { nowMs: 1060, leaseMs: 100 });
  assert.equal(second.attempts, 2);
  assert.equal(queue.fail('job', second.leaseToken, 'still unavailable', { nowMs: 1061 }), true);
  assert.equal(queue.get('job').status, 'dead');
  assert.equal(queue.enqueue('job', { overwritten: true }, { nowMs: 1100 }).inserted, false);
  assert.equal(queue.claim('worker', { nowMs: 1100 }), undefined);
  assert.equal(queue.retryDead('job', { nowMs: 1200 }), true);
  assert.equal(queue.get('job').attempts, 0);
  assert.deepEqual(queue.get('job').payload, { report: 42 });
  const cursor = database.stats().latestCursor;
  assert.equal(queue.retryDead('job', { nowMs: 1200 }), false);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(queue.claim('worker', { nowMs: 1200 }).attempts, 1);
});

test('lease extension retains ownership until its new deadline', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', null, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  assert.equal(queue.extend('job', 'wrong-token', { nowMs: 1010, leaseMs: 100 }), false);
  assert.equal(queue.extend('job', leased.leaseToken, { nowMs: 1010, leaseMs: 100 }), true);
  assert.equal(queue.get('job').leaseUntil, 1110);
  const cursor = database.stats().latestCursor;
  assert.equal(queue.extend('job', leased.leaseToken, { nowMs: 1011, leaseMs: 1 }), true);
  assert.equal(queue.get('job').leaseUntil, 1110);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(queue.claim('other-worker', { nowMs: 1100 }), undefined);
  assert.equal(queue.ack('job', leased.leaseToken, { nowMs: 1109 }), true);
});

test('ack commits completion, business records and outbox writes in one frame', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', { total: 25 }, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  const cursor = database.stats().latestCursor;
  assert.equal(queue.ack('job', leased.leaseToken, {
    nowMs: 1001,
    operations: [
      { kind: 'put', collection: 'results', key: 'job', value: { total: 25 } },
      { kind: 'put', collection: 'outbox', key: 'job:done', value: { type: 'report.ready' } },
    ],
  }), true);
  assert.equal(queue.get('job').status, 'done');
  assert.deepEqual(database.collection('results').get('job'), { total: 25 });
  const history = database.changes({ since: cursor });
  assert.equal(history.transactions.length, 1);
  assert.equal(history.transactions[0].operations.length, 3);
  assert.equal(queue.enqueue('job', null, { nowMs: 1002 }).inserted, false);
});

test('invalid acknowledgment writes leave ownership and application state unchanged', (t) => {
  const { database, queue } = fixture(t, { maxDocumentBytes: 1024 });
  queue.enqueue('job', true, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  const cursor = database.stats().latestCursor;
  assert.throws(() => queue.ack('job', leased.leaseToken, {
    nowMs: 1001,
    operations: [
      { kind: 'put', collection: 'results', key: 'first', value: true },
      { kind: 'put', collection: 'results', key: 'large', value: { text: 'x'.repeat(2000) } },
    ],
  }), /too large/);
  assert.equal(queue.get('job').status, 'leased');
  assert.equal(database.collection('results').get('first'), undefined);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(queue.ack('job', leased.leaseToken, { nowMs: 1001 }), true);
});

test('the combined acknowledgment frame is bounded and fails without partial writes', (t) => {
  const { database, queue } = fixture(t, { maxDocumentBytes: 2048, maxFrameBytes: 1024 });
  queue.enqueue('job', true, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  const cursor = database.stats().latestCursor;
  assert.throws(() => queue.ack('job', leased.leaseToken, {
    nowMs: 1001,
    operations: [{ kind: 'put', collection: 'results', key: 'large', value: 'x'.repeat(900) }],
  }), /frame/i);
  assert.equal(queue.get('job').status, 'leased');
  assert.equal(database.collection('results').get('large'), undefined);
  assert.equal(database.stats().latestCursor, cursor);
});

test('job metadata counts toward document limits before enqueue commits', (t) => {
  const { database, queue } = fixture(t, { maxDocumentBytes: 64 });
  assert.throws(() => queue.enqueue('job', null, { nowMs: 1000 }), /too large/);
  assert.equal(queue.get('job'), undefined);
  assert.equal(database.stats().latestCursor, 0);
});

test('bounded expired recovery requeues remaining attempts and dead-letters exhausted leases', (t) => {
  const { queue } = fixture(t);
  queue.enqueue('a', true, { nowMs: 1000, maxAttempts: 1 });
  queue.enqueue('b', true, { nowMs: 1000, maxAttempts: 2 });
  queue.enqueue('c', true, { nowMs: 1000, maxAttempts: 2 });
  queue.claim('worker', { nowMs: 1000, leaseMs: 10 });
  queue.claim('worker', { nowMs: 1000, leaseMs: 10 });
  queue.claim('worker', { nowMs: 1000, leaseMs: 10 });
  assert.equal(queue.stats({ nowMs: 1010 }).expired, 3);
  assert.deepEqual(queue.recoverExpired({ nowMs: 1010, limit: 1 }), { requeued: 0, dead: 1, hasMore: true });
  assert.deepEqual(queue.recoverExpired({ nowMs: 1010, limit: 10 }), { requeued: 2, dead: 0, hasMore: false });
  assert.deepEqual(queue.stats({ nowMs: 1010 }), { queued: 2, leased: 0, done: 0, dead: 1, ready: 2, expired: 0 });
});

test('an expired final attempt requires recovery before becoming dead', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', null, { nowMs: 1000, maxAttempts: 1 });
  queue.claim('worker', { nowMs: 1000, leaseMs: 10 });
  const cursor = database.stats().latestCursor;
  assert.equal(queue.claim('worker', { nowMs: 1010 }), undefined);
  assert.equal(queue.get('job').status, 'leased');
  assert.equal(database.stats().latestCursor, cursor);
  assert.deepEqual(queue.recoverExpired({ nowMs: 1010 }), { requeued: 0, dead: 1, hasMore: false });
});

test('reopening preserves leases, deduplication and atomic acknowledgment results', (t) => {
  const { database, directory, queue } = fixture(t);
  queue.enqueue('job', { n: 1 }, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  database.close();
  const reopened = new Database(directory);
  try {
    const restoredQueue = reopened.queue('reports');
    assert.equal(restoredQueue.get('job').leaseToken, leased.leaseToken);
    assert.equal(restoredQueue.enqueue('job', { n: 2 }, { nowMs: 1001 }).inserted, false);
    assert.equal(restoredQueue.ack('job', leased.leaseToken, {
      nowMs: 1001, operations: [{ kind: 'put', collection: 'results', key: 'job', value: 1 }],
    }), true);
    assert.equal(reopened.collection('results').get('job'), 1);
    assert.equal(restoredQueue.get('job').status, 'done');
  } finally { reopened.close(); }
});

test('backups preserve queue state and fencing within the restored history', (t) => {
  const { database, queue, root } = fixture(t);
  queue.enqueue('job', { n: 1 }, { nowMs: 1000 });
  const leased = queue.claim('worker:1', { nowMs: 1000, leaseMs: 100 });
  const backup = path.join(root, 'queue.backup');
  database.backup(backup);
  const destination = path.join(root, 'restored');
  Database.restoreBackup(backup, destination);
  const restored = new Database(destination);
  try {
    const restoredQueue = restored.queue('reports');
    assert.equal(restoredQueue.get('job').leaseToken, leased.leaseToken);
    assert.equal(restoredQueue.ack('job', leased.leaseToken, { nowMs: 1100 }), false);
    const reclaimed = restoredQueue.claim('worker:2', { nowMs: 1100, leaseMs: 100 });
    assert.notEqual(reclaimed.leaseToken, leased.leaseToken);
    assert.equal(restoredQueue.ack('job', leased.leaseToken, { nowMs: 1101 }), false);
    assert.equal(restoredQueue.ack('job', reclaimed.leaseToken, { nowMs: 1101 }), true);
    assert.equal(restored.verify().records, 1);
  } finally { restored.close(); }
});

test('queue records reject direct mutations and cross-queue acknowledgment writes', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', true, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  const reserved = '_cedro.jobs.reports';
  const cursor = database.stats().latestCursor;
  assert.throws(() => database.collection(reserved).put('job', { malformed: true }));
  assert.throws(() => database.collection(reserved).delete('job'));
  assert.throws(() => database.collection(reserved).compareExchange('job', undefined, null));
  assert.throws(() => database.conditionalBatch({
    conditions: [], operations: [{ kind: 'delete', collection: reserved, key: 'job' }],
  }));
  assert.throws(() => queue.ack('job', leased.leaseToken, {
    nowMs: 1001, operations: [{ kind: 'delete', collection: '_cedro.jobs.other', key: 'job' }],
  }), /reserved/);
  assert.equal(database.stats().latestCursor, cursor);
  assert.equal(queue.get('job').status, 'leased');
});

test('invalid payloads, identifiers and time arithmetic fail before mutation', (t) => {
  const { database, queue } = fixture(t);
  const cycle = {}; cycle.self = cycle;
  for (const payload of [undefined, 1n, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, { x: undefined }, cycle, new Date(), [, 1], '\ud800']) {
    assert.throws(() => queue.enqueue('invalid', payload, { nowMs: 1000 }));
  }
  for (const name of ['', 'has space', '🌲', 'a'.repeat(129), 'nul\0name']) assert.throws(() => database.queue(name));
  for (const id of ['', 'nul\0id', 'x'.repeat(4097), '\ud800']) assert.throws(() => queue.enqueue(id, null, { nowMs: 1000 }));
  for (const options of [{ nowMs: -1 }, { nowMs: 1.5 }, { nowMs: 1n }, { nowMs: null }, { maxAttempts: 0 }, { maxAttempts: 1001 }, { priority: 1001 }, { delayMs: -1 }, { typo: 1 }, { nowMs: Number.MAX_SAFE_INTEGER, delayMs: 1 }]) {
    assert.throws(() => queue.enqueue('invalid', null, options));
  }
  assert.throws(() => queue.claim('x'.repeat(129), { nowMs: 1000 }));
  assert.throws(() => queue.claim('worker', { nowMs: 1000, leaseMs: 0 }));
  assert.throws(() => queue.claim('worker', { nowMs: Number.MAX_SAFE_INTEGER, leaseMs: 1 }));
  assert.throws(() => queue.recoverExpired({ limit: 0, nowMs: 1000 }));
  assert.throws(() => queue.recoverExpired({ limit: 1001, nowMs: 1000 }));
  assert.equal(database.stats().latestCursor, 0);
  queue.enqueue('safe', { largeId: String(Number.MAX_SAFE_INTEGER + 1) }, { nowMs: 1000 });
});

test('raw queue bridge rejects unsafe numbers and unknown fields before claiming', (t) => {
  const { database, directory, queue } = fixture(t);
  queue.enqueue('job', null, { nowMs: 1000 });
  database.close();
  const { NativeDatabase } = require('../native.cjs');
  const native = new NativeDatabase(directory, '{}');
  try {
    assert.throws(() => native.queueJson('{"action":"claim","name":"reports","worker":"worker","now_ms":9007199254740993,"lease_ms":1}'), /safe numeric/);
    assert.throws(() => native.queueJson('{"action":"claim","name":"reports","worker":"worker","now_ms":1000,"lease_ms":1,"unknown":true}'));
    assert.throws(() => native.queueJson('{"action":"claim","name":"reports","worker":"worker","now_ms":9007199254740991,"lease_ms":1}'));
    assert.throws(() => native.queueJson('{"action":"claim","name":"reports","worker":"worker","now_ms":1000,"lease_ms":0}'));
    assert.equal(JSON.parse(native.stats()).latest_cursor, 1);
    assert.equal(JSON.parse(native.queueJson('{"action":"get","name":"reports","id":"job"}')).status, 'queued');
  } finally { native.close(); }
});

test('errors and wrong tokens preserve ownership; close invalidates every queue handle', (t) => {
  const { database, queue } = fixture(t);
  queue.enqueue('job', null, { nowMs: 1000 });
  const leased = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  for (const error of ['', 'nul\0error', '🌲'.repeat(1025)]) assert.throws(() => queue.fail('job', leased.leaseToken, error, { nowMs: 1001 }));
  assert.throws(() => queue.ack('job', '', { nowMs: 1001 }));
  assert.throws(() => queue.ack('job', leased.leaseToken, { nowMs: 1001, operations: [{ kind: 'put', collection: 'results', key: 'job', value: 1n }] }));
  assert.equal(queue.get('job').status, 'leased');
  assert.equal(queue.ack('job', 'arbitrary-wrong-token', { nowMs: 1001 }), false);
  database.close();
  assert.throws(() => queue.get('job'), /closed/);
  assert.throws(() => queue.claim('worker', { nowMs: 1001 }), /closed/);
  assert.throws(() => queue.enqueue('new', null, { nowMs: 1001 }), /closed/);
});

test('ESM and CommonJS export the same Queue constructor', async () => {
  const esm = await import('../index.mjs');
  assert.equal(esm.Queue, Queue);
  assert.equal(esm.default.Queue, Queue);
});
