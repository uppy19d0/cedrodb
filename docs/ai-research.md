# CedroDB for local AI applications

Research checked against primary documentation on 2026-10-15. This is a product
direction and an implementation rationale, not evidence of market adoption or
superiority. Adoption must be tested with real applications and users.

## Needs supported by existing frameworks

LangGraph separates per-thread graph checkpoints from cross-thread application
memory. This supports a distinction between recovering execution state and
retrieving useful information for a later conversation.
[LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence).

Its checkpointer contract also preserves task writes, parents and serializer
information. A snapshot JSON key/value API does not implement that protocol.
CedroDB 0.6 therefore exposes its own checkpoint store rather than claiming to
be a drop-in saver. [Checkpointers](https://docs.langchain.com/oss/python/langgraph/checkpointers).

AI SDK message persistence retains full UI messages, including tool/data parts
and stable IDs, and recommends validating restored messages against current
schemas. Storage and resuming a streamed generation are different concerns.
[AI SDK message persistence](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-message-persistence).

PydanticAI deferred tools distinguish human approvals from external results.
Resumption needs the corresponding request identities and conversation state;
an application must preserve those and enforce its own authorization.
[Deferred tools](https://pydantic.dev/docs/ai/tools-toolsets/deferred-tools/).

## Existing alternatives

| Existing option | Relevant capability | Implication for CedroDB |
| --- | --- | --- |
| SQLite + sqlite-vec | Local transactions, full-text facilities and metadata-aware vector queries | Local persistence or vectors alone do not differentiate us |
| LanceDB | Embedded retrieval with vector, lexical and hybrid ranking | BM25/cosine/RRF are established techniques, not a unique invention |
| LangGraph persistence | Checkpointers and cross-thread stores with defined integration contracts | A supported adapter needs conformance tests and pinned framework versions |
| Mem0 | A higher-level memory pipeline using configured model/storage components | Storing documents/vectors does not provide automatic fact extraction or memory quality |

Sources: [SQLite transactions](https://www.sqlite.org/transactional.html),
[FTS5](https://www.sqlite.org/fts5.html),
[sqlite-vec metadata](https://alexgarcia.xyz/sqlite-vec/features/vec0.html),
[LanceDB hybrid search](https://docs.lancedb.com/search/hybrid-search),
[LangGraph stores](https://docs.langchain.com/oss/python/langgraph/stores),
[Mem0 quickstart](https://docs.mem0.ai/open-source/python-quickstart).

## Chosen 0.6 application

A local support assistant, desktop copilot or offline integration needs to keep
its state after restart, find relevant local records, and avoid publishing a
result belonging to an obsolete continuation. CedroDB can combine these needs
with its existing native SDKs, JSON transaction log, backups and outbox.

The shipped building blocks are revision-checked checkpoints with ordinary
document/outbox writes in the same frame, and budgeted retrieval by text,
supplied vectors or both. Prefilters, model-version metadata, deterministic
ties and separate scoring details make retrieval inspectable. Real examples
exercise restart, stale-write rejection and atomic output publication; their
synthetic embeddings do not measure answer quality.

The proposed differentiation is ergonomic integration of operational state
and local retrieval across Rust, Node and Python. This is an engineering
positioning decision. We do not claim a new retrieval algorithm, execution
engine, production guarantee, or better performance than established systems.

## Tested framework recipe

The optional Python conversation recipe was executed with Python 3.12.15 and
PydanticAI 2.56.0, the published version checked for this release. It serializes
with `ConversationTypeAdapter`, closes/reopens CedroDB, then continues using a
fresh deterministic `TestModel` agent. Assertions preserve the conversation ID,
advance messages from two to four and usage requests from one to two, and save
the continued conversation and outbox together. It makes no model-service calls.
This validates the recipe, not a PydanticAI durable-execution backend or every
conversation/tool payload. The base SDK still supports Python 3.9+ and adds no
framework dependency.

## Evidence and adoption work still needed

1. Publish reproducible latency, memory, reopen and unfavorable-workload results;
   exact scans and RAM-resident documents have clear size/concurrency boundaries.
2. Test a LangGraph Store adapter against released versions and upstream
   semantics before attempting its more involved checkpointer protocol.
3. Evaluate a scoped MCP interface using the official SDK and real client
   interoperability tests; define resource ownership and authorization first.
4. Build an application with real embeddings and a fixed retrieval evaluation
   set, comparing lexical, vector and hybrid modes without fabricated metrics.
5. Establish history retention/compaction and compatibility guarantees before
   recommending long-running production agents.

These are future tasks, not implemented capabilities. The storage and release
gates in [the roadmap](roadmap.md) remain requirements before stable 1.0.
