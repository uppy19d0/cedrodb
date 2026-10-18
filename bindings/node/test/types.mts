import cedrodb, { Database, Queue, AgentStore, type Query, type JsonValue, type QueueStats, type Job, type SearchPage, type AgentCheckpoint, type Diagnostics, type QueryProfile, type SearchProfile } from 'cedrodb';
const db = new Database('./unused-typecheck');
const jobs = db.collection<{ state: string; score: number }>('jobs');
const query: Query = { filters: [{ field: 'state', op: 'eq', value: 'pending' }] };
const values: JsonValue[] = jobs.find(query).map(({ value }) => value);
const page = jobs.findPage({ after: 'a', limit: 10 });
const next: string | undefined = page.nextAfter;
const cursor: number = db.changes().nextCursor;
const same: typeof Database = cedrodb.Database;
const verified: number = db.verify().transactions;
const backedUp: number = db.backup('./new-backup.cedro').logBytes;
const restored: number = Database.restoreBackup('./new-backup.cedro', './new-data', { maxStateBytes: 1024 }).records;
const conditional = db.conditionalBatch({
  conditions: [{ kind: 'absent', collection: 'orders', key: 'order:1' }],
  operations: [{ kind: 'put', collection: 'orders', key: 'order:1', value: { state: 'paid' } }],
});
const failed: number | null = conditional.failedCondition;
const queue = db.queue<{ task: string }>('reports');
const queued: Job<{ task: string }> = queue.enqueue('job:1', { task: 'report' }, { nowMs: 1000, priority: 2 }).job;
const queueConstructor: typeof Queue = cedrodb.Queue;
const queueStats: QueueStats = queue.stats({ nowMs: 1000 });
const search: SearchPage = db.collection('knowledge').search({ text: 'cedro', vector: [1, 0], maxScanned: 1000, minVectorScore: 0 });
const agents = db.agent<{ messages: string[] }>('support');
const agentConstructor: typeof AgentStore = cedrodb.AgentStore;
const saved = agents.checkpoint('thread', { messages: ['hello'] }, { expectedRevision: null, nowMs: 1000, status: 'waiting', metadata: { source: 'test' } });
const checkpoint: AgentCheckpoint<{ messages: string[] }> | undefined = agents.get('thread');
const diagnostics: Diagnostics = db.diagnostics();
const queryProfile: QueryProfile<{ state: string; score: number }> = jobs.profileQuery(query);
const searchProfile: SearchProfile<{ state: string; score: number }> = jobs.profileSearch({ vector: [1, 0] });
const profiledState: string | undefined = queryProfile.page.records[0]?.value.state;
const profiledScore: number | undefined = searchProfile.page.hits[0]?.value.score;
if (saved.applied) agents.checkpoint('thread', { messages: ['hello', 'world'] }, { expectedRevision: saved.checkpoint.revision });
const lease = queue.claim('worker:1', { nowMs: 1000, leaseMs: 100 });
if (lease) {
  const payload: string = lease.payload.task;
  queue.extend(lease.id, lease.leaseToken, { nowMs: 1010, leaseMs: 100 });
  queue.ack(lease.id, lease.leaseToken, { nowMs: 1020, operations: [{ kind: 'put', collection: 'results', key: lease.id, value: payload }] });
}
void [values, next, cursor, same, verified, backedUp, restored, failed, queued, queueConstructor, queueStats, search, checkpoint, agentConstructor, diagnostics, queryProfile, searchProfile, profiledState, profiledScore];
// @ts-expect-error BigInt is not a JSON document.
jobs.put('a', 1n);
// @ts-expect-error Query operator is a closed set.
jobs.find({ filters: [{ field: 'score', op: 'contains', value: 1 }] });
// @ts-expect-error absent is a distinct condition without a value field.
db.conditionalBatch({ conditions: [{ kind: 'absent', collection: 'c', key: 'a', value: null }], operations: [] });
// @ts-expect-error Queue payloads use the declared generic type.
queue.enqueue('job:2', { task: 42 });
// @ts-expect-error Timestamps and durations use safe integer numbers, not BigInt.
queue.claim('worker:2', { leaseMs: 1n });
// @ts-expect-error Search embeddings are arrays of numbers.
db.collection('knowledge').search({ vector: ['invalid'] });
// @ts-expect-error Application approval is not a built-in checkpoint status.
agents.checkpoint('thread', { messages: [] }, { status: 'approved' });
// @ts-expect-error Agent states obey their declared generic shape.
agents.checkpoint('thread', { messages: [42] });
// @ts-expect-error Profiling uses the same closed query option type.
jobs.profileQuery({ maxScanned: 10 });
// @ts-expect-error A search profile requires search options.
jobs.profileSearch();
const code: 'CEDRO_CLOSED' = cedrodb.ErrorCode.CLOSED;
