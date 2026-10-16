# Agent checkpoints and searchable memory

CedroDB 0.6 adds native checkpoint storage and local retrieval to its JSON,
outbox, queue and backup APIs. It does not execute models or workflows. The same
Rust implementation serves Node.js and Python; no model service is required.

## Save a checkpoint and its results together

```js
const agents = db.agent('support');
const previous = agents.get('conversation:42');
const saved = agents.checkpoint('conversation:42', {
  messages: [{ role: 'assistant', content: 'Your answer is ready.' }],
  requestId: 'request:42'
}, {
  expectedRevision: previous?.revision ?? null,
  status: 'completed',
  metadata: { schema: 'support-v1' },
  operations: [{
    kind: 'put', collection: 'outbox', key: 'request:42:answered',
    value: { eventId: 'request:42:answered', type: 'answer.ready' }
  }]
});
if (!saved.applied) {
  // Another continuation won. Inspect saved.checkpoint and recompute.
}
```

Python uses `db.agent(namespace).checkpoint(thread_id, state,
expected_revision=..., status=..., metadata=..., operations=...)`. See the
[complete Python example](../bindings/python/examples/agent_memory.py).

An omitted/null expected revision means **create only if absent**. Updating
requires the exact committed revision. Revision is an opaque positive safe
integer equal to the transaction's global durable cursor, not a per-thread
counter. Unrelated transactions can leave gaps. A conflict returns the current
checkpoint and cursor, appends nothing, and applies none of the ordinary writes.

The stored checkpoint contains `schema_version`, `thread_id`, `revision`,
`status`, `state`, `metadata` and `updated_at`. Node uses camelCase field names.
State is portable JSON; metadata must be an object. Both reject numbers outside
the JavaScript-safe range ±(2^53−1), including oversized floating-point values.
Store large IDs and exact decimal quantities as strings. The checkpoint envelope
also consumes document/frame/state capacity, so an otherwise valid state may
need more headroom. All limits fail before publishing any part of the commit.

Namespaces contain 1–128 ASCII letters/digits, dots, underscores or dashes.
Thread IDs contain 1–4,096 UTF-8 bytes without NUL. Separate namespace/thread
components do not alias. `_cedro.agents.*` collections are reserved against
generic writes, CAS, conditional batches and queue-ack operations. Existing
data using this prefix must be migrated with an older release before upgrading.
Internal records are readable through ordinary APIs, but only typed checkpoint
operations may update them.

`running`, `waiting` and `completed` are application-managed status values.
Setting `waiting` does not request approval, and setting `completed` does not
finish a queue job. The application authenticates approvers, validates tool
arguments, executes effects and determines legal status transitions. Checkpoint
and queue ACK/enqueue are separate commits. Use a durable outbox dispatcher to
schedule follow-up work and deduplicate delivery; do not infer a workflow engine
or exactly-once external execution from these storage APIs.

## Retrieve memory by text, vectors or both

```python
memory = db.collection('support-memory')
memory.create_index('tenant')
page = memory.search(
    text='refund policy', vector=[1.0, 0.0],  # Synthetic example vector.
    vector_field='embedding', text_fields=['text', '/document/title'],
    filters=[{'field': 'tenant', 'op': 'eq', 'value': 'shop-1'},
             {'field': 'model', 'op': 'eq', 'value': 'demo-v1'}],
    min_vector_score=0.2, limit=5, max_scanned=10000,
)
for hit in page['hits']:
    print(hit['key'], hit['score'], hit['vector_score'], hit['text_score'])
```

Node calls `collection.search({ text, vector, vectorField, textFields, filters,
minVectorScore, limit, maxScanned })` and returns camelCase result fields.
Defaults are `embedding`, `['text']`, no filters, limit 10 and scan budget 10,000.
At least one of `text` or `vector` is required. Hits include the complete stored
document, so omit secrets from the collection or redact them before exposing a
result to a model/client.

| Mode | Ranking | Score contract |
| --- | --- | --- |
| Text | BM25, k1=1.2 and b=0.75 | Sum over distinct query terms; positive matches only |
| Vector | Exact cosine over supplied vectors | Floating-point score in [-1, 1] |
| Hybrid | Reciprocal rank fusion, k=60 | Sum of `1 / (60 + rank)` over matching channels |

Ranks start at one. Each complete channel is ranked before applying the final
limit; lexical nonmatches do not receive a lexical rank. Equal scores use key
order, including channel ties. A hybrid hit can match only one channel.
`min_vector_score` gates the vector channel; it does not exclude an independent
text match in hybrid mode. It requires a vector and a finite value in [-1, 1].
Raw text and vector scores remain available separately; scores from different
modes or different filtered corpora are not comparable relevance probabilities.

Text tokenization splits on non-alphanumeric Unicode characters and lowercases
terms. There is no stemming, stop-word removal, accent folding, phrase query or
language-specific analyzer. Duplicate query terms count once. Text fields are
concatenated for document length/frequencies. Missing/non-string text fields
are ignored. BM25 statistics use eligible documents with nonempty tokenized
text, including documents with no query-term match.

Vectors contain 1–4,096 finite numbers with nonzero magnitude. Normalization
scales first to handle extreme finite magnitudes. Missing vector fields are
ignored; present null, nonnumeric, zero or differently dimensioned vectors in
eligible documents cause an error. Filter by **embedding model/version**, not
just dimension: equally sized vectors from different models are not comparable.
CedroDB does not generate, refresh or certify embeddings. Example two-dimensional
vectors are deterministic test data, not measured model quality.

## Scope, budgets and consistency

All metadata filters run before vector validation/scoring and lexical corpus
statistics. Filters are the same AND predicates/JSON Pointers as ordinary
queries. An applicable equality index narrows candidates before scanning;
recreate indexes after reopening. `scanned` counts visited candidates before
filters; `matched` counts documents matching at least one retrieval channel.

| Input | Limit |
| --- | --- |
| Returned hits | 1–1,000 |
| Scan budget | 1–100,000; default 10,000 |
| Query text | 1–4,096 UTF-8 bytes, 1–64 distinct alphanumeric terms |
| Unique text fields | 1–8 |
| Metadata filters | At most 64 |

Exceeding the scan budget fails the whole query. There is no silent candidate
truncation or claim of a globally exact result after a partial scan. Narrow
filters, add an equality index or explicitly raise the budget.

One search holds the engine mutex and observes committed state. It does not
write a frame or expose staged writes. Retrieval scans vectors/text and sorts
rankings; it is O(n·d + n log n), needs working memory proportional to candidates
and query terms, and blocks other operations on the handle. It is not ANN/HNSW,
an inverted full-text index, a distributed vector service or a data-larger-than-
RAM engine. Benchmark representative workloads before choosing it.

Namespaces and filters help applications keep data scoped; they do not
authenticate callers or prevent code holding the database handle from selecting
another namespace/collection. Authorize access before calling the SDK. Retrieved
content remains untrusted input to an agent.

Backups retain complete document/checkpoint history. Deleting a memory document
does not physically remove its prior log/backup versions. There is no automatic
retention, compaction or certified erasure. Restoring/forking a backup can reuse
future revisions: isolate old workers and reconcile external effects.

## Framework interoperability

This is an application-facing JSON API, not a LangGraph checkpointer or a
PydanticAI durable-execution backend. Framework state can include binary values,
pending task writes, versioned serializers and outstanding tool requests. Use
the framework's supported serialization, record its version, and validate on
restore. Persisting completed chat messages does not resume an interrupted
generation or recover framework usage budgets.

The optional [PydanticAI history example](../bindings/python/examples/pydantic_ai_history.py)
uses a deterministic test model and a pinned dependency. It demonstrates the
contract described in that file; it adds no dependency to CedroDB itself.
Research and the next integration priorities are documented in
[the AI research note](ai-research.md).
