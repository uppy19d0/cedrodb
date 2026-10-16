'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { Database, AgentStore } = require('..');

function fixture(t, options) {
  const root = fs.mkdtempSync(path.join(tmpdir(), 'cedrodb-node-agent-'));
  const directory = path.join(root, 'database');
  const database = new Database(directory, options);
  t.after(() => { database.close(); fs.rmSync(root, { recursive: true, force: true }); });
  return { root, directory, database, agents: database.agent('support') };
}

test('AgentStore stores portable state with camelCase checkpoint fields and defaults', (t) => {
  const { database, agents } = fixture(t);
  assert.ok(agents instanceof AgentStore);
  assert.equal(agents.namespace, 'support');
  assert.equal(agents.get('missing'), undefined);
  const result = agents.checkpoint('thread:1', { messages: [{ role: 'user', parts: [{ type: 'text', text: 'Hola 🌲' }] }] }, { nowMs: 1000 });
  assert.equal(result.applied, true);
  assert.equal(result.cursor, 1);
  assert.deepEqual(result.checkpoint, {
    schemaVersion: 1, threadId: 'thread:1', revision: 1, status: 'running',
    state: { messages: [{ role: 'user', parts: [{ type: 'text', text: 'Hola 🌲' }] }] },
    metadata: {}, updatedAt: 1000,
  });
  result.checkpoint.state.messages[0].parts[0].text = 'changed';
  assert.equal(agents.get('thread:1').state.messages[0].parts[0].text, 'Hola 🌲');
  assert.equal(database.stats().latestCursor, 1);
});

test('null or omitted expectedRevision means create only; conflicts return current state', (t) => {
  const { database, agents } = fixture(t);
  const first = agents.checkpoint('thread', null, { nowMs: 1000 });
  for (const options of [{ nowMs: 1001 }, { expectedRevision: null, nowMs: 1001 }]) {
    assert.deepEqual(agents.checkpoint('thread', 'overwrite', options), {
      applied: false, cursor: first.cursor, checkpoint: first.checkpoint,
    });
  }
  assert.deepEqual(agents.checkpoint('absent', true, { expectedRevision: first.checkpoint.revision, nowMs: 1001 }), {
    applied: false, cursor: first.cursor, checkpoint: null,
  });
  assert.equal(database.stats().latestCursor, first.cursor);
});

test('a stale continuation cannot overwrite a checkpoint or its application/outbox writes', (t) => {
  const { database, agents } = fixture(t);
  const first = agents.checkpoint('thread', { step: 1 }, { nowMs: 1000 }).checkpoint;
  const next = agents.checkpoint('thread', { step: 2 }, {
    expectedRevision: first.revision, nowMs: 1001, status: 'waiting', metadata: { reason: 'approval' },
    operations: [
      { kind: 'put', collection: 'tool_results', key: 'request:1', value: { result: 42 } },
      { kind: 'put', collection: 'outbox', key: 'request:1', value: { type: 'approval.requested' } },
    ],
  });
  assert.equal(next.applied, true);
  assert.equal(next.checkpoint.status, 'waiting');
  const cursor = next.cursor;
  const stale = agents.checkpoint('thread', { step: 'stale' }, {
    expectedRevision: first.revision, nowMs: 1002,
    operations: [{ kind: 'put', collection: 'tool_results', key: 'request:1', value: 'stale' }],
  });
  assert.deepEqual(stale, { applied: false, cursor, checkpoint: next.checkpoint });
  assert.deepEqual(database.collection('tool_results').get('request:1'), { result: 42 });
  const changes = database.changes({ since: first.revision });
  assert.equal(changes.transactions.length, 1);
  assert.equal(changes.transactions[0].operations.length, 3);
});

test('checkpoint revisions are opaque durable cursors and may skip unrelated commits', (t) => {
  const { database, agents } = fixture(t);
  const first = agents.checkpoint('thread', true, { nowMs: 1000 }).checkpoint;
  database.collection('other').put('a', 1);
  const second = agents.checkpoint('thread', false, { expectedRevision: first.revision, nowMs: 1001 });
  assert.equal(second.applied, true);
  assert.equal(second.checkpoint.revision, 3);
  assert.equal(second.cursor, database.stats().latestCursor);
  assert.notEqual(second.checkpoint.revision, first.revision + 1);
});

test('checkpoint and business writes roll back atomically when a document or frame exceeds limits', (t) => {
  const { database, agents } = fixture(t, { maxDocumentBytes: 2048, maxFrameBytes: 1024 });
  const first = agents.checkpoint('thread', { step: 1 }, { nowMs: 1000 }).checkpoint;
  const cursor = database.stats().latestCursor;
  assert.throws(() => agents.checkpoint('thread', { step: 2 }, {
    expectedRevision: first.revision, nowMs: 1001,
    operations: [
      { kind: 'put', collection: 'results', key: 'first', value: true },
      { kind: 'put', collection: 'results', key: 'large', value: 'x'.repeat(900) },
    ],
  }), /frame/);
  assert.deepEqual(agents.get('thread'), first);
  assert.equal(database.collection('results').get('first'), undefined);
  assert.equal(database.stats().latestCursor, cursor);
  assert.throws(() => agents.checkpoint('thread', 'ignored', {
    nowMs: 1002, operations: [{ kind: 'put', collection: 'results', key: 'large', value: 'x'.repeat(3000) }],
  }), /too large/); // Input must remain valid even if the creation guard would conflict.
});

test('namespaces isolate same-named threads and reserved agent records reject generic writes', (t) => {
  const { database, agents } = fixture(t);
  agents.checkpoint('thread', 'support', { nowMs: 1000 });
  database.agent('other').checkpoint('thread', 'other', { nowMs: 1000 });
  assert.equal(agents.get('thread').state, 'support');
  assert.equal(database.agent('other').get('thread').state, 'other');
  const reserved = '_cedro.agents.support';
  assert.throws(() => database.collection(reserved).put('thread', null));
  assert.throws(() => database.collection(reserved).delete('thread'));
  assert.throws(() => database.collection(reserved).compareExchange('thread', undefined, null));
  assert.throws(() => database.conditionalBatch({ conditions: [], operations: [{ kind: 'delete', collection: reserved, key: 'thread' }] }));
  assert.throws(() => agents.checkpoint('new', true, {
    operations: [{ kind: 'delete', collection: reserved, key: 'thread' }], nowMs: 1001,
  }), /reserved/);
  assert.throws(() => agents.checkpoint('new', true, {
    operations: [{ kind: 'delete', collection: '_cedro.jobs.jobs', key: 'job' }], nowMs: 1001,
  }), /reserved/);
  const queue = database.queue('jobs');
  queue.enqueue('job', null, { nowMs: 1000 });
  const lease = queue.claim('worker', { nowMs: 1000, leaseMs: 100 });
  assert.throws(() => queue.ack('job', lease.leaseToken, {
    nowMs: 1001, operations: [{ kind: 'delete', collection: reserved, key: 'thread' }],
  }));
  assert.equal(queue.get('job').status, 'leased');
});

test('reopening and verified backups preserve agent revisions and complete state', (t) => {
  const { root, directory, database, agents } = fixture(t);
  const saved = agents.checkpoint('thread', { step: 1 }, { nowMs: 1000, status: 'waiting', metadata: { tool: 'review' } }).checkpoint;
  const backup = path.join(root, 'agent.backup');
  database.backup(backup);
  agents.checkpoint('thread', { step: 2 }, { expectedRevision: saved.revision, nowMs: 1001, status: 'completed' });
  database.close();
  const reopened = new Database(directory);
  try { assert.equal(reopened.agent('support').get('thread').status, 'completed'); } finally { reopened.close(); }
  const destination = path.join(root, 'restored');
  Database.restoreBackup(backup, destination);
  const restored = new Database(destination);
  try {
    assert.deepEqual(restored.agent('support').get('thread'), saved);
    const next = restored.agent('support').checkpoint('thread', { step: 2 }, { expectedRevision: saved.revision, nowMs: 1002 });
    assert.equal(next.applied, true);
    assert.equal(next.checkpoint.revision, saved.revision + 1);
    assert.equal(restored.verify().transactions, 2);
  } finally { restored.close(); }
});

test('checkpoint decimal values retain their exact JavaScript representation across restart', (t) => {
  const { database, directory, agents } = fixture(t);
  const decimal = 0.19450526354013234;
  const saved = agents.checkpoint('decimal', { score: decimal }, { nowMs: 1000, metadata: { threshold: decimal } });
  assert.equal(saved.checkpoint.state.score, decimal);
  assert.equal(saved.checkpoint.metadata.threshold, decimal);
  database.close();
  const reopened = new Database(directory);
  try {
    const restored = reopened.agent('support').get('decimal');
    assert.equal(restored.state.score, decimal);
    assert.equal(restored.metadata.threshold, decimal);
  } finally { reopened.close(); }
});

test('agent inputs reject unsafe state, invalid metadata/options and bad identifiers before committing', (t) => {
  const { database, agents } = fixture(t);
  for (const namespace of ['', 'a b', 'a'.repeat(129), '🌲']) assert.throws(() => database.agent(namespace));
  for (const thread of ['', 'x'.repeat(4097), 'nul\0id', '\ud800']) assert.throws(() => agents.checkpoint(thread, null));
  for (const state of [undefined, 1n, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, { x: undefined }, new Date(), [, 1]]) {
    assert.throws(() => agents.checkpoint('invalid', state, { nowMs: 1000 }));
  }
  for (const options of [
    { metadata: null }, { metadata: [] }, { metadata: { unsafe: 1n } },
    { nowMs: -1 }, { nowMs: null }, { nowMs: Number.MAX_SAFE_INTEGER + 1 },
    { status: 'approved' }, { expectedRevision: 0 }, { expectedRevision: -1 },
    { expectedRevision: 1n }, { operations: null }, { typo: true },
  ]) assert.throws(() => agents.checkpoint('invalid', null, options));
  assert.equal(database.stats().latestCursor, 0);
});

test('native agent bridge validates numeric ranges and strict request fields before mutation', (t) => {
  const { database, directory } = fixture(t);
  database.close();
  const { NativeDatabase } = require('../native.cjs');
  const native = new NativeDatabase(directory, '{}');
  try {
    const request = { action: 'checkpoint', namespace: 'support', thread_id: 'thread', state: true, expected_revision: null, now_ms: 1000, status: 'running', metadata: {}, operations: [] };
    assert.throws(() => native.agentJson(JSON.stringify({ ...request, now_ms: Number.MAX_SAFE_INTEGER + 1 })), /safe numeric/);
    assert.throws(() => native.agentJson(JSON.stringify({ ...request, unknown: true })));
    assert.equal(JSON.parse(native.stats()).latest_cursor, 0);
  } finally { native.close(); }
});

test('closed handles fail and ESM/CommonJS expose the same AgentStore', async (t) => {
  const { database, agents } = fixture(t);
  const esm = await import('../index.mjs');
  assert.equal(esm.AgentStore, AgentStore);
  assert.equal(esm.default.AgentStore, AgentStore);
  database.close();
  assert.throws(() => agents.get('thread'), /closed/);
  assert.throws(() => agents.checkpoint('thread', null, { nowMs: 1000 }), /closed/);
});
