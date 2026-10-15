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
export type Condition =
  | { kind: 'absent'; collection: string; key: string }
  | { kind: 'equals'; collection: string; key: string; value: JsonValue };
export interface ConditionalBatchRequest { conditions: Condition[]; operations: BatchOperation[] }
export interface ConditionalResult { applied: boolean; failedCondition: number | null; cursor: number }
export interface ChangeTransaction { cursor: number; operations: BatchOperation[] }
export interface ChangePage { transactions: ChangeTransaction[]; nextCursor: number; hasMore: boolean }
export interface Stats {
  version: string; records: number; collections: number; contentBytes: number;
  logBytes: number; latestCursor: number; indexes: number;
}
export interface Aggregate { count: number; sum: number; min: number | null; max: number | null }
/** Verified persisted log contents; counters exclude session indexes. */
export interface IntegrityReport { logBytes: number; transactions: number; records: number; contentBytes: number }
export type JobStatus = 'queued' | 'leased' | 'done' | 'dead';
export interface Job<T extends JsonValue = JsonValue> {
  schemaVersion: 1; id: string; payload: T; status: JobStatus;
  attempts: number; maxAttempts: number; priority: number;
  createdAt: number; availableAt: number; leaseUntil: number | null;
  leaseToken: string | null; worker: string | null; lastError: string | null;
}
export interface LeasedJob<T extends JsonValue = JsonValue> extends Job<T> {
  status: 'leased'; leaseUntil: number; leaseToken: string; worker: string;
}
export interface EnqueueOptions { nowMs?: number; delayMs?: number; maxAttempts?: number; priority?: number }
export interface EnqueueResult<T extends JsonValue = JsonValue> { inserted: boolean; job: Job<T> }
export interface LeaseOptions { nowMs?: number; leaseMs?: number }
export interface AckOptions { nowMs?: number; operations?: BatchOperation[] }
export interface FailOptions { nowMs?: number; retryDelayMs?: number }
export interface QueueStats { queued: number; leased: number; done: number; dead: number; ready: number; expired: number }
export interface RecoveryResult { requeued: number; dead: number; hasMore: boolean }

/** All operations are synchronous and block the calling Node.js thread. */
export class Database {
  constructor(path: string, options?: DatabaseOptions);
  /** Validate a backup and restore it into a new directory. Existing targets are rejected. */
  static restoreBackup(backupPath: string, destinationPath: string, options?: DatabaseOptions): IntegrityReport;
  collection<T extends JsonValue = JsonValue>(name: string): Collection<T>;
  queue<T extends JsonValue = JsonValue>(name: string): Queue<T>;
  /** Idempotently close the native handle and release its exclusive file lock. */
  close(): void;
  /** Commit all operations together, or commit none if validation fails. */
  batch(operations: BatchOperation[]): void;
  /** Check up to 64 guards against committed state, then commit one atomic batch if all match. */
  conditionalBatch(request: ConditionalBatchRequest): ConditionalResult;
  /** since is an exclusive durable transaction cursor; limits count transactions. */
  changes(options?: { since?: number; limit?: number }): ChangePage;
  stats(): Stats;
  /** Strictly verify every persisted frame without modifying or repairing the log. */
  verify(): IntegrityReport;
  /** Write a verified backup container to a new file. Existing files are rejected. */
  backup(path: string): IntegrityReport;
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
/** Durable local queue; methods scan current queue state synchronously. */
export class Queue<T extends JsonValue = JsonValue> {
  constructor(database: Database, name: string);
  readonly name: string;
  /** An existing ID, including done/dead jobs, returns inserted=false without replacing its payload. */
  enqueue(id: string, payload: T, options?: EnqueueOptions): EnqueueResult<T>;
  claim(worker: string, options?: LeaseOptions): LeasedJob<T> | undefined;
  /** Complete the leased job and optional business/outbox writes in one durable transaction. */
  ack(id: string, token: string, options?: AckOptions): boolean;
  fail(id: string, token: string, error: string, options?: FailOptions): boolean;
  extend(id: string, token: string, options?: LeaseOptions): boolean;
  get(id: string): Job<T> | undefined;
  stats(options?: { nowMs?: number }): QueueStats;
  recoverExpired(options?: { nowMs?: number; limit?: number }): RecoveryResult;
  retryDead(id: string, options?: { nowMs?: number }): boolean;
}
export const version: '0.5.0';
