/** JSON numbers must be finite and |value| <= Number.MAX_SAFE_INTEGER. */
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export interface DatabaseOptions {
  maxDocumentBytes?: number;
  maxFrameBytes?: number;
  maxStateBytes?: number;
}
export interface Record<T extends JsonValue = JsonValue> { key: string; value: T }
export type FilterOp = 'eq' | 'ne' | 'gt' | 'gte' | 'lt' | 'lte' | 'exists';
/** Fields are top-level names or JSON Pointers, e.g. /profile/city. exists requires a boolean. */
export interface Filter { field: string; op: FilterOp; value: JsonValue }
export interface Query { filters?: Filter[]; limit?: number; after?: string }
export interface QueryPage<T extends JsonValue = JsonValue> {
  records: Record<T>[];
  nextAfter?: string;
  scanned: number;
  usedIndex?: string;
}
export type BatchOperation =
  | { kind: 'put'; collection: string; key: string; value: JsonValue }
  | { kind: 'delete'; collection: string; key: string };
export interface ChangeTransaction { cursor: number; operations: BatchOperation[] }
export interface ChangePage { transactions: ChangeTransaction[]; nextCursor: number; hasMore: boolean }
export interface Stats {
  version: string; records: number; collections: number; contentBytes: number;
  logBytes: number; latestCursor: number; indexes: number;
}
export interface Aggregate { count: number; sum: number; min: number | null; max: number | null }

/** All operations are synchronous and block the calling Node.js thread. */
export class Database {
  constructor(path: string, options?: DatabaseOptions);
  collection<T extends JsonValue = JsonValue>(name: string): Collection<T>;
  /** Idempotently close the native handle and release its exclusive file lock. */
  close(): void;
  /** Commit all operations together, or commit none if validation fails. */
  batch(operations: BatchOperation[]): void;
  /** since is an exclusive durable transaction cursor; limits count transactions. */
  changes(options?: { since?: number; limit?: number }): ChangePage;
  stats(): Stats;
}
export class Collection<T extends JsonValue = JsonValue> {
  constructor(database: Database, name: string);
  readonly name: string;
  put(key: string, value: T): void;
  /** undefined means absent; null is a stored JSON value. */
  get(key: string): T | undefined;
  delete(key: string): void;
  putMany(records: Record<T>[]): void;
  /** Return one bounded page, default 100 / maximum 1000 records. */
  find(query?: Query): Record<T>[];
  findPage(query?: Query): QueryPage<T>;
  /** Session index; recreate after reopening. Equality filters can use it. */
  createIndex(field: string): void;
  /** undefined expected means absent; undefined replacement deletes the record. */
  compareExchange(key: string, expected: T | undefined, replacement: T | undefined): boolean;
  /** Numeric summary over a top-level field; floating-point arithmetic. */
  aggregate(field: string): Aggregate;
}
export const version: '0.2.0';
