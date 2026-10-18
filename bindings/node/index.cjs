'use strict';
const { NativeDatabase } = require('./native.cjs');

const own = (value, key) => Object.prototype.hasOwnProperty.call(value, key);
const OPS = new Set(['eq', 'ne', 'gt', 'gte', 'lt', 'lte', 'exists']);

function string(value, label) {
  if (typeof value !== 'string' || value.length === 0 || value.includes('\0')) {
    throw new TypeError(`${label} must be a nonempty string without NUL characters`);
  }
  wellFormed(value, label);
  return value;
}

function wellFormed(value, label) {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(++index);
      if (!(next >= 0xdc00 && next <= 0xdfff)) throw new TypeError(`${label} contains an unpaired surrogate`);
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw new TypeError(`${label} contains an unpaired surrogate`);
    }
  }
}

function object(value, label, keys) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)
      || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) {
    throw new TypeError(`${label} must be a plain object`);
  }
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== 'string' || (keys && !keys.includes(key))) {
      throw new TypeError(`Unknown ${label} property: ${String(key)}`);
    }
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor.enumerable || !own(descriptor, 'value')) {
      throw new TypeError(`${label} properties must be enumerable data values`);
    }
  }
  return value;
}

function integer(value, label, minimum = 0, maximum = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new RangeError(`${label} must be a safe integer in ${minimum}..${maximum}`);
  }
  return value;
}

// JSON.stringify alone silently drops undefined/functions and changes NaN to
// null. Validate first so the value acknowledged by the caller is the stored one.
function validateJSON(value, ancestors = new Set(), depth = 0) {
  if (depth > 64) throw new RangeError('JSON document nesting exceeds 64 levels');
  if (value === null || typeof value === 'boolean') return;
  if (typeof value === 'string') return wellFormed(value, 'JSON string');
  if (typeof value === 'number') {
    if (!Number.isFinite(value) || Math.abs(value) > Number.MAX_SAFE_INTEGER) {
      throw new RangeError('JSON numbers must be finite and within the safe numeric range; use strings for large identifiers');
    }
    return;
  }
  if (typeof value !== 'object') throw new TypeError('Only JSON values are supported; undefined and BigInt are not document values');
  if (ancestors.has(value)) throw new TypeError('JSON document contains a cycle');
  ancestors.add(value);
  if (Array.isArray(value)) {
    if (Object.getPrototypeOf(value) !== Array.prototype || 'toJSON' in value) {
      throw new TypeError('JSON arrays must use Array.prototype and have no toJSON property');
    }
    const keys = Reflect.ownKeys(value).filter((key) => key !== 'length');
    if (keys.length !== value.length) throw new TypeError('JSON arrays must be dense and have no extra properties');
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !own(descriptor, 'value')) {
        throw new TypeError('JSON arrays must contain enumerable data values');
      }
      validateJSON(descriptor.value, ancestors, depth + 1);
    }
  } else {
    object(value, 'JSON object');
    for (const key of Object.keys(value)) {
      wellFormed(key, 'JSON object key');
      validateJSON(value[key], ancestors, depth + 1);
    }
  }
  ancestors.delete(value);
}

function json(value) {
  validateJSON(value);
  return JSON.stringify(value);
}

function normalizeQuery(input = {}) {
  object(input, 'query', ['filters', 'limit', 'after']);
  const filters = input.filters ?? [];
  if (!Array.isArray(filters)) throw new TypeError('query.filters must be an array');
  const normalized = filters.map((filter) => {
    object(filter, 'filter', ['field', 'op', 'value']);
    string(filter.field, 'filter.field');
    if (!OPS.has(filter.op)) throw new TypeError('Unsupported filter operator');
    if (!own(filter, 'value')) throw new TypeError('Every filter requires a value');
    if (filter.op === 'exists' && typeof filter.value !== 'boolean') throw new TypeError('exists requires a boolean value');
    validateJSON(filter.value);
    return { field: filter.field, op: filter.op, value: filter.value };
  });
  return JSON.stringify({
    filters: normalized,
    limit: integer(input.limit ?? 100, 'query.limit', 1, 1000),
    after: input.after === undefined ? null : string(input.after, 'query.after'),
  });
}

function normalizeSearch(input) {
  object(input, 'search', ['text', 'vector', 'vectorField', 'textFields', 'filters', 'limit', 'maxScanned', 'minVectorScore']);
  const text = own(input, 'text') ? boundedString(input.text, 'search.text', 4096) : null;
  let vector = null;
  if (own(input, 'vector')) {
    if (!Array.isArray(input.vector) || input.vector.length === 0) throw new TypeError('search.vector must be a nonempty array of numbers');
    if (input.vector.length > 4096) throw new RangeError('search.vector supports at most 4096 dimensions');
    validateJSON(input.vector);
    for (const value of input.vector) {
      if (typeof value !== 'number') throw new TypeError('search.vector must contain only numbers');
    }
    vector = input.vector;
  }
  if (text === null && vector === null) throw new TypeError('search requires text, vector, or both');
  const textFields = own(input, 'textFields') ? input.textFields : ['text'];
  if (!Array.isArray(textFields)) throw new TypeError('search.textFields must be an array');
  validateJSON(textFields);
  const fields = Array.from(textFields, (field) => string(field, 'search.textFields entry'));
  if (fields.length === 0 || fields.length > 8 || new Set(fields).size !== fields.length) {
    throw new RangeError('search.textFields requires 1..8 unique fields');
  }
  if (own(input, 'filters') && !Array.isArray(input.filters)) throw new TypeError('search.filters must be an array');
  const base = JSON.parse(normalizeQuery({ filters: own(input, 'filters') ? input.filters : [] }));
  let minimum = null;
  if (own(input, 'minVectorScore')) {
    minimum = input.minVectorScore;
    if (typeof minimum !== 'number' || !Number.isFinite(minimum) || minimum < -1 || minimum > 1) {
      throw new RangeError('minVectorScore must be a finite number in -1..1');
    }
  }
  return JSON.stringify({
    text, vector,
    vector_field: own(input, 'vectorField') ? string(input.vectorField, 'search.vectorField') : 'embedding',
    text_fields: fields, filters: base.filters,
    limit: integer(own(input, 'limit') ? input.limit : 10, 'search.limit', 1, 1000),
    max_scanned: integer(own(input, 'maxScanned') ? input.maxScanned : 10000, 'search.maxScanned', 1, 100000),
    min_vector_score: minimum,
  });
}

function normalizeOperation(operation) {
  object(operation, 'batch operation', ['kind', 'collection', 'key', 'value']);
  const collection = string(operation.collection, 'operation.collection');
  const key = string(operation.key, 'operation.key');
  if (operation.kind === 'delete') {
    if (own(operation, 'value')) throw new TypeError('delete operations cannot have a value');
    return { kind: 'delete', collection, key };
  }
  if (operation.kind !== 'put' || !own(operation, 'value')) throw new TypeError('Operation must be put with a value, or delete');
  validateJSON(operation.value);
  return { kind: 'put', collection, key, value: operation.value };
}

function normalizeCondition(condition) {
  object(condition, 'condition', ['kind', 'collection', 'key', 'value']);
  const collection = string(condition.collection, 'condition.collection');
  const key = string(condition.key, 'condition.key');
  if (condition.kind === 'absent') {
    if (own(condition, 'value')) throw new TypeError('absent conditions cannot have a value');
    return { kind: 'absent', collection, key };
  }
  if (condition.kind !== 'equals' || !own(condition, 'value')) {
    throw new TypeError('Condition must be absent, or equals with a value');
  }
  validateJSON(condition.value);
  return { kind: 'equals', collection, key, value: condition.value };
}

function normalizeOptions(options = {}) {
  object(options, 'options', ['maxDocumentBytes', 'maxFrameBytes', 'maxStateBytes']);
  const normalized = {};
  for (const [publicName, engineName] of [
    ['maxDocumentBytes', 'max_document_bytes'], ['maxFrameBytes', 'max_frame_bytes'], ['maxStateBytes', 'max_state_bytes'],
  ]) {
    if (own(options, publicName)) normalized[engineName] = integer(options[publicName], publicName, 1);
  }
  return JSON.stringify(normalized);
}

function integrityReport(raw) {
  const report = JSON.parse(raw);
  return {
    logBytes: report.log_bytes, transactions: report.transactions,
    records: report.records, contentBytes: report.content_bytes,
  };
}

function queueName(name) {
  if (typeof name !== 'string' || !/^[A-Za-z0-9_.-]{1,128}$/.test(name)) {
    throw new TypeError('Queue name must contain 1..128 ASCII letters, digits, underscores, dots or hyphens');
  }
  return name;
}

function boundedString(value, label, maximumBytes) {
  string(value, label);
  if (Buffer.byteLength(value, 'utf8') > maximumBytes) throw new RangeError(`${label} exceeds ${maximumBytes} UTF-8 bytes`);
  return value;
}

function queueInteger(options, name, fallback, minimum = 0, maximum = Number.MAX_SAFE_INTEGER) {
  return integer(own(options, name) ? options[name] : fallback, name, minimum, maximum);
}

function jobRecord(job) {
  if (job == null) return undefined;
  return {
    schemaVersion: job.schema_version, id: job.id, payload: job.payload, status: job.status,
    attempts: job.attempts, maxAttempts: job.max_attempts, priority: job.priority,
    createdAt: job.created_at, availableAt: job.available_at,
    leaseUntil: job.lease_until, leaseToken: job.lease_token, worker: job.worker, lastError: job.last_error,
  };
}

function agentNamespace(namespace) {
  if (typeof namespace !== 'string' || !/^[A-Za-z0-9_.-]{1,128}$/.test(namespace)) {
    throw new TypeError('Agent namespace must contain 1..128 ASCII letters, digits, underscores, dots or hyphens');
  }
  return namespace;
}

function agentRecord(checkpoint) {
  if (checkpoint == null) return undefined;
  return {
    schemaVersion: checkpoint.schema_version, threadId: checkpoint.thread_id,
    revision: checkpoint.revision, status: checkpoint.status, state: checkpoint.state,
    metadata: checkpoint.metadata, updatedAt: checkpoint.updated_at,
  };
}

function statsRecord(result) {
  return {
    version: result.version, records: result.records, collections: result.collections,
    contentBytes: result.content_bytes, logBytes: result.log_bytes,
    latestCursor: result.latest_cursor, indexes: result.indexes,
  };
}

function queryPage(page) {
  return {
    records: page.records, nextAfter: page.next_after ?? undefined,
    scanned: page.scanned, usedIndex: page.used_index ?? undefined,
  };
}

function searchPage(page) {
  return {
    hits: page.hits.map((hit) => ({
      key: hit.key, value: hit.value, score: hit.score,
      vectorScore: hit.vector_score, textScore: hit.text_score,
    })),
    scanned: page.scanned, matched: page.matched, usedIndex: page.used_index ?? undefined, mode: page.mode,
  };
}

function profilePlan(plan) {
  return {
    collectionRecords: plan.collection_records, candidateRecords: plan.candidate_records,
    scanned: plan.scanned, returned: plan.returned, usedIndex: plan.used_index ?? undefined,
  };
}

function operationMetrics(metrics) {
  return {
    attempts: metrics.attempts, successes: metrics.successes, failures: metrics.failures,
    totalElapsedMicros: metrics.total_elapsed_micros, maxElapsedMicros: metrics.max_elapsed_micros,
  };
}

class Database {
  #native;

  constructor(path, options = {}) {
    string(path, 'path');
    this.#native = new NativeDatabase(path, normalizeOptions(options));
  }

  static restoreBackup(backupPath, destinationPath, options = {}) {
    return integrityReport(NativeDatabase.restoreBackup(
      string(backupPath, 'backupPath'), string(destinationPath, 'destinationPath'), normalizeOptions(options),
    ));
  }

  collection(name) { return new Collection(this, string(name, 'collection name')); }

  queue(name) { return new Queue(this, queueName(name)); }

  agent(namespace) { return new AgentStore(this, agentNamespace(namespace)); }

  close() { this.#native.close(); }

  batch(operations) {
    if (!Array.isArray(operations)) throw new TypeError('batch requires an array');
    // Array.from visits holes, which normalizeOperation rejects before commit.
    this.#native.batch(JSON.stringify(Array.from(operations, normalizeOperation)));
  }

  conditionalBatch(request) {
    object(request, 'conditional batch', ['conditions', 'operations']);
    if (!Array.isArray(request.conditions) || !Array.isArray(request.operations)) {
      throw new TypeError('conditionalBatch requires conditions and operations arrays');
    }
    if (request.conditions.length > 64) throw new RangeError('conditionalBatch supports at most 64 conditions');
    const normalized = {
      conditions: Array.from(request.conditions, normalizeCondition),
      operations: Array.from(request.operations, normalizeOperation),
    };
    const result = JSON.parse(this.#native.conditionalBatchJson(JSON.stringify(normalized)));
    return { applied: result.applied, failedCondition: result.failed_condition ?? null, cursor: result.cursor };
  }

  changes(options = {}) {
    object(options, 'changes options', ['since', 'limit']);
    const page = JSON.parse(this.#native.changes(
      String(integer(options.since ?? 0, 'since')), integer(options.limit ?? 100, 'limit', 1, 1000),
    ));
    return { transactions: page.transactions, nextCursor: page.next_cursor, hasMore: page.has_more };
  }

  stats() { return statsRecord(JSON.parse(this.#native.stats())); }

  diagnostics() {
    const report = JSON.parse(this.#native.diagnostics());
    return {
      stats: statsRecord(report.stats),
      limits: {
        maxDocumentBytes: report.limits.max_document_bytes,
        maxFrameBytes: report.limits.max_frame_bytes,
        maxStateBytes: report.limits.max_state_bytes,
      },
      collections: report.collections.map((collection) => ({
        collection: collection.collection, records: collection.records,
        contentBytes: collection.content_bytes, indexFields: collection.index_fields,
      })),
      session: {
        query: operationMetrics(report.session.query), search: operationMetrics(report.session.search),
        commit: operationMetrics(report.session.commit),
      },
      poisoned: report.poisoned, writerActive: report.writer_active,
    };
  }

  verify() { return integrityReport(this.#native.verify()); }

  backup(path) { return integrityReport(this.#native.backup(string(path, 'backup path'))); }

  _queue(request) { return JSON.parse(this.#native.queueJson(JSON.stringify(request))); }

  _agent(request) { return JSON.parse(this.#native.agentJson(JSON.stringify(request))); }

  _search(collection, query) {
    const page = JSON.parse(this.#native.searchJson(collection, normalizeSearch(query)));
    return searchPage(page);
  }

  _profileQuery(collection, query) {
    const profile = JSON.parse(this.#native.profileQueryJson(collection, normalizeQuery(query)));
    return { page: queryPage(profile.page), elapsedMicros: profile.elapsed_micros, plan: profilePlan(profile.plan) };
  }

  _profileSearch(collection, query) {
    const profile = JSON.parse(this.#native.profileSearchJson(collection, normalizeSearch(query)));
    return { page: searchPage(profile.page), elapsedMicros: profile.elapsed_micros, plan: profilePlan(profile.plan) };
  }

  _get(collection, key) {
    const raw = this.#native.get(collection, string(key, 'key'));
    return raw == null ? undefined : JSON.parse(raw);
  }

  _findPage(collection, query) {
    const page = JSON.parse(this.#native.findPage(collection, normalizeQuery(query)));
    return queryPage(page);
  }

  _createIndex(collection, field) { this.#native.createIndex(collection, string(field, 'field')); }

  _compareExchange(collection, key, expected, replacement) {
    return this.#native.compareExchange(collection, string(key, 'key'),
      expected === undefined ? undefined : json(expected), replacement === undefined ? undefined : json(replacement));
  }

  _aggregate(collection, field) { return JSON.parse(this.#native.aggregate(collection, string(field, 'field'))); }
}

class Collection {
  #database;
  #name;

  constructor(database, name) {
    if (!(database instanceof Database)) throw new TypeError('Collection requires a Database');
    this.#database = database;
    this.#name = string(name, 'collection name');
  }

  get name() { return this.#name; }
  get(key) { return this.#database._get(this.#name, key); }
  put(key, value) { this.#database.batch([{ kind: 'put', collection: this.#name, key, value }]); }
  delete(key) { this.#database.batch([{ kind: 'delete', collection: this.#name, key }]); }

  putMany(records) {
    if (!Array.isArray(records)) throw new TypeError('putMany requires an array of {key, value}');
    const operations = Array.from(records, (record) => {
      object(record, 'record', ['key', 'value']);
      if (!own(record, 'value')) throw new TypeError('Every record requires a value');
      return { kind: 'put', collection: this.#name, key: record.key, value: record.value };
    });
    this.#database.batch(operations);
  }

  find(query = {}) { return this.findPage(query).records; }
  findPage(query = {}) { return this.#database._findPage(this.#name, query); }
  search(query) { return this.#database._search(this.#name, query); }
  profileQuery(query = {}) { return this.#database._profileQuery(this.#name, query); }
  profileSearch(query) { return this.#database._profileSearch(this.#name, query); }
  createIndex(field) { this.#database._createIndex(this.#name, field); }
  compareExchange(key, expected, replacement) { return this.#database._compareExchange(this.#name, key, expected, replacement); }
  aggregate(field) { return this.#database._aggregate(this.#name, field); }
}

class AgentStore {
  #database;
  #namespace;

  constructor(database, namespace) {
    if (!(database instanceof Database)) throw new TypeError('AgentStore requires a Database');
    this.#database = database;
    this.#namespace = agentNamespace(namespace);
  }

  get namespace() { return this.#namespace; }

  get(threadId) {
    return agentRecord(this.#database._agent({
      action: 'get', namespace: this.#namespace, thread_id: boundedString(threadId, 'threadId', 4096),
    }));
  }

  checkpoint(threadId, state, options = {}) {
    boundedString(threadId, 'threadId', 4096);
    validateJSON(state);
    object(options, 'checkpoint options', ['expectedRevision', 'nowMs', 'status', 'metadata', 'operations']);
    const expected = options.expectedRevision === undefined || options.expectedRevision === null
      ? null : integer(options.expectedRevision, 'expectedRevision', 1);
    const status = own(options, 'status') ? options.status : 'running';
    if (!['running', 'waiting', 'completed'].includes(status)) throw new TypeError('Unsupported agent checkpoint status');
    const metadata = own(options, 'metadata') ? options.metadata : {};
    object(metadata, 'checkpoint metadata');
    validateJSON(metadata);
    const operations = own(options, 'operations') ? options.operations : [];
    if (!Array.isArray(operations)) throw new TypeError('checkpoint operations must be an array');
    const normalized = Array.from(operations, normalizeOperation);
    for (const operation of normalized) {
      if (operation.collection.startsWith('_cedro.agents.') || operation.collection.startsWith('_cedro.jobs.')) {
        throw new TypeError('checkpoint operations cannot modify reserved agent or queue collections');
      }
    }
    const result = this.#database._agent({
      action: 'checkpoint', namespace: this.#namespace, thread_id: threadId, state,
      expected_revision: expected, now_ms: queueInteger(options, 'nowMs', Date.now()), status, metadata,
      operations: normalized,
    });
    return {
      applied: result.applied, cursor: result.cursor,
      checkpoint: result.checkpoint == null ? null : agentRecord(result.checkpoint),
    };
  }
}

class Queue {
  #database;
  #name;

  constructor(database, name) {
    if (!(database instanceof Database)) throw new TypeError('Queue requires a Database');
    this.#database = database;
    this.#name = queueName(name);
  }

  get name() { return this.#name; }

  enqueue(id, payload, options = {}) {
    boundedString(id, 'job id', 4096);
    validateJSON(payload);
    object(options, 'enqueue options', ['nowMs', 'delayMs', 'maxAttempts', 'priority']);
    const now = queueInteger(options, 'nowMs', Date.now());
    const delay = queueInteger(options, 'delayMs', 0);
    integer(now + delay, 'available time');
    const result = this.#database._queue({
      action: 'enqueue', name: this.#name, id, payload, now_ms: now, delay_ms: delay,
      max_attempts: queueInteger(options, 'maxAttempts', 3, 1, 1000),
      priority: queueInteger(options, 'priority', 0, 0, 1000),
    });
    return { inserted: result.inserted, job: jobRecord(result.job) };
  }

  claim(worker, options = {}) {
    boundedString(worker, 'worker', 128);
    object(options, 'claim options', ['nowMs', 'leaseMs']);
    const now = queueInteger(options, 'nowMs', Date.now());
    const lease = queueInteger(options, 'leaseMs', 30000, 1);
    integer(now + lease, 'lease expiry');
    return jobRecord(this.#database._queue({ action: 'claim', name: this.#name, worker, now_ms: now, lease_ms: lease }));
  }

  ack(id, token, options = {}) {
    boundedString(id, 'job id', 4096);
    string(token, 'lease token');
    object(options, 'ack options', ['nowMs', 'operations']);
    const operations = own(options, 'operations') ? options.operations : [];
    if (!Array.isArray(operations)) throw new TypeError('ack operations must be an array');
    const normalized = Array.from(operations, normalizeOperation);
    for (const operation of normalized) {
      if (operation.collection.startsWith('_cedro.jobs.')) throw new TypeError('ack operations cannot modify the reserved queue namespace');
    }
    return this.#database._queue({
      action: 'ack', name: this.#name, id, token,
      now_ms: queueInteger(options, 'nowMs', Date.now()), operations: normalized,
    });
  }

  fail(id, token, error, options = {}) {
    boundedString(id, 'job id', 4096);
    string(token, 'lease token');
    boundedString(error, 'job error', 4096);
    object(options, 'fail options', ['nowMs', 'retryDelayMs']);
    const now = queueInteger(options, 'nowMs', Date.now());
    const delay = queueInteger(options, 'retryDelayMs', 0);
    integer(now + delay, 'retry time');
    return this.#database._queue({ action: 'fail', name: this.#name, id, token, error, now_ms: now, retry_delay_ms: delay });
  }

  extend(id, token, options = {}) {
    boundedString(id, 'job id', 4096);
    string(token, 'lease token');
    object(options, 'extend options', ['nowMs', 'leaseMs']);
    const now = queueInteger(options, 'nowMs', Date.now());
    const lease = queueInteger(options, 'leaseMs', 30000, 1);
    integer(now + lease, 'lease expiry');
    return this.#database._queue({ action: 'extend', name: this.#name, id, token, now_ms: now, lease_ms: lease });
  }

  get(id) {
    return jobRecord(this.#database._queue({ action: 'get', name: this.#name, id: boundedString(id, 'job id', 4096) }));
  }

  stats(options = {}) {
    object(options, 'queue stats options', ['nowMs']);
    return this.#database._queue({ action: 'stats', name: this.#name, now_ms: queueInteger(options, 'nowMs', Date.now()) });
  }

  recoverExpired(options = {}) {
    object(options, 'recovery options', ['nowMs', 'limit']);
    const result = this.#database._queue({
      action: 'recover_expired', name: this.#name,
      now_ms: queueInteger(options, 'nowMs', Date.now()), limit: queueInteger(options, 'limit', 10, 1, 1000),
    });
    return { requeued: result.requeued, dead: result.dead, hasMore: result.has_more };
  }

  retryDead(id, options = {}) {
    object(options, 'retry options', ['nowMs']);
    return this.#database._queue({
      action: 'retry_dead', name: this.#name, id: boundedString(id, 'job id', 4096),
      now_ms: queueInteger(options, 'nowMs', Date.now()),
    });
  }
}

// Stable `error.code` values for errors raised by the native engine. Messages are unchanged.
const ErrorCode = Object.freeze({
  IO: 'CEDRO_IO',
  SERIALIZATION: 'CEDRO_SERIALIZATION',
  CORRUPTION: 'CEDRO_CORRUPTION',
  LIMIT_EXCEEDED: 'CEDRO_LIMIT_EXCEEDED',
  INVALID_INPUT: 'CEDRO_INVALID_INPUT',
  DATABASE_LOCKED: 'CEDRO_DATABASE_LOCKED',
  WRITER_BUSY: 'CEDRO_WRITER_BUSY',
  POISONED: 'CEDRO_POISONED',
  CLOSED: 'CEDRO_CLOSED',
  UNKNOWN: 'CEDRO_UNKNOWN',
});

module.exports = { Database, Collection, Queue, AgentStore, ErrorCode, version: '0.9.0' };
