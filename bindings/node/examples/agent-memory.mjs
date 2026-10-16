import { Database } from '../index.mjs';

// These hand-written 2D vectors demonstrate API behavior. They are not model
// embeddings, semantic quality measurements or performance benchmarks.
const db = new Database(process.argv[2] ?? `./agent-memory-${Date.now()}`);
try {
  const knowledge = db.collection('knowledge');
  knowledge.putMany([
    { key: 'backups', value: { tenant: 'alice', embeddingModel: 'synthetic-2d', text: 'Cedro backups preserve the complete transaction history.', embedding: [1, 0] } },
    { key: 'jobs', value: { tenant: 'alice', embeddingModel: 'synthetic-2d', text: 'Local jobs use leases, retries and durable acknowledgments.', embedding: [0.2, 0.9] } },
    { key: 'another-tenant', value: { tenant: 'bob', embeddingModel: 'synthetic-3d', text: 'Unrelated document', embedding: [0, 0, 1] } },
  ]);
  knowledge.createIndex('embeddingModel');

  const agents = db.agent('assistant');
  const threadId = 'alice:conversation:42';
  const messages = [{ id: 'user:1', role: 'user', parts: [{ type: 'text', text: 'Explain Cedro backups' }] }];
  const created = agents.checkpoint(threadId, { messages, step: 'retrieve' }, {
    nowMs: 1000, metadata: { embeddingModel: 'synthetic-2d' },
  });
  if (!created.applied) throw new Error('Use a new database directory for this example');

  const results = knowledge.search({
    text: 'backups history', vector: [1, 0],
    filters: [
      { field: 'tenant', op: 'eq', value: 'alice' },
      { field: 'embeddingModel', op: 'eq', value: 'synthetic-2d' },
    ],
    limit: 2, maxScanned: 100,
  });
  console.log('Local retrieval:', results);

  const saved = agents.checkpoint(threadId, {
    messages,
    step: 'review',
    retrievedKeys: results.hits.map((hit) => hit.key),
  }, {
    expectedRevision: created.checkpoint.revision,
    nowMs: 1001, status: 'waiting', metadata: { approvalRequired: true },
    operations: [
      { kind: 'put', collection: 'tool_results', key: 'retrieval:42', value: { keys: results.hits.map((hit) => hit.key) } },
      { kind: 'put', collection: 'outbox', key: 'review:42', value: { type: 'agent.review_requested', threadId } },
    ],
  });
  console.log('Checkpoint and outbox committed:', saved.applied, saved.cursor);

  const stale = agents.checkpoint(threadId, { step: 'stale' }, {
    expectedRevision: created.checkpoint.revision, nowMs: 1002,
    operations: [{ kind: 'put', collection: 'outbox', key: 'stale:42', value: true }],
  });
  console.log('Stale continuation applied:', stale.applied);
  console.log('Stale event exists:', db.collection('outbox').get('stale:42'));
  console.log('Persisted checkpoint:', agents.get(threadId));
  // The application owns approval, model calls, execution and external delivery.
} finally {
  db.close();
}
