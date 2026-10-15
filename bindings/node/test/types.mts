import cedrodb, { Database, type Query, type JsonValue } from 'cedrodb';
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
void [values, next, cursor, same, verified, backedUp, restored, failed];
// @ts-expect-error BigInt is not a JSON document.
jobs.put('a', 1n);
// @ts-expect-error Query operator is a closed set.
jobs.find({ filters: [{ field: 'score', op: 'contains', value: 1 }] });
// @ts-expect-error absent is a distinct condition without a value field.
db.conditionalBatch({ conditions: [{ kind: 'absent', collection: 'c', key: 'a', value: null }], operations: [] });
