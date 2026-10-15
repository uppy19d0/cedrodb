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

class Database {
  #native;

  constructor(path, options = {}) {
    string(path, 'path');
    object(options, 'options', ['maxDocumentBytes', 'maxFrameBytes', 'maxStateBytes']);
    const normalized = {};
    for (const [publicName, engineName] of [
      ['maxDocumentBytes', 'max_document_bytes'], ['maxFrameBytes', 'max_frame_bytes'], ['maxStateBytes', 'max_state_bytes'],
    ]) {
      if (own(options, publicName)) normalized[engineName] = integer(options[publicName], publicName, 1);
    }
    this.#native = new NativeDatabase(path, JSON.stringify(normalized));
  }

  collection(name) { return new Collection(this, string(name, 'collection name')); }

  close() { this.#native.close(); }

  batch(operations) {
    if (!Array.isArray(operations)) throw new TypeError('batch requires an array');
    // Array.from visits holes, which normalizeOperation rejects before commit.
    this.#native.batch(JSON.stringify(Array.from(operations, normalizeOperation)));
  }

  changes(options = {}) {
    object(options, 'changes options', ['since', 'limit']);
    const page = JSON.parse(this.#native.changes(
      String(integer(options.since ?? 0, 'since')), integer(options.limit ?? 100, 'limit', 1, 1000),
    ));
    return { transactions: page.transactions, nextCursor: page.next_cursor, hasMore: page.has_more };
  }

  stats() {
    const result = JSON.parse(this.#native.stats());
    return {
      version: result.version, records: result.records, collections: result.collections,
      contentBytes: result.content_bytes, logBytes: result.log_bytes,
      latestCursor: result.latest_cursor, indexes: result.indexes,
    };
  }

  _get(collection, key) {
    const raw = this.#native.get(collection, string(key, 'key'));
    return raw == null ? undefined : JSON.parse(raw);
  }

  _findPage(collection, query) {
    const page = JSON.parse(this.#native.findPage(collection, normalizeQuery(query)));
    return { records: page.records, nextAfter: page.next_after ?? undefined, scanned: page.scanned, usedIndex: page.used_index ?? undefined };
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
  createIndex(field) { this.#database._createIndex(this.#name, field); }
  compareExchange(key, expected, replacement) { return this.#database._compareExchange(this.#name, key, expected, replacement); }
  aggregate(field) { return this.#database._aggregate(this.#name, field); }
}

module.exports = { Database, Collection, version: '0.2.0' };
