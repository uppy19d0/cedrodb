import cedrodb, { Database, type Query, type JsonValue } from 'cedrodb';
const db = new Database('./unused-typecheck');
const jobs = db.collection<{ state: string; score: number }>('jobs');
const query: Query = { filters: [{ field: 'state', op: 'eq', value: 'pending' }] };
const values: JsonValue[] = jobs.find(query).map(({ value }) => value);
const page = jobs.findPage({ after: 'a', limit: 10 });
const next: string | undefined = page.nextAfter;
const cursor: number = db.changes().nextCursor;
const same: typeof Database = cedrodb.Database;
void [values, next, cursor, same];
// @ts-expect-error BigInt is not a JSON document.
jobs.put('a', 1n);
// @ts-expect-error Query operator is a closed set.
jobs.find({ filters: [{ field: 'score', op: 'contains', value: 1 }] });
