# Knowledge Graph Plan: Hindsight, Graphiti, and OpenCode Providers

Status: Proposed, documentation only

Research snapshot: 2026-07-29

Scope: design review before implementation. This document does not change the
Rust daemon, Protobuf contract, TypeScript plugin, dependencies, or persisted
state.

## Executive Recommendation

Build a knowledge-base graph as an additive layer on top of the existing native
memory system. Do not replace zvec, lifecycle state, Markdown-backed repository
memory, or the current local embedding path.

The recommended split is:

```text
OpenCode process
  TypeScript plugin
    -> optional OpenCode provider bridge for LLM extraction
    -> validated extraction candidates
    -> native graph RPC

Native Rust daemon
  ProjectActor
    -> graph validation and entity resolution
    -> graph sidecar persistence and journal
    -> existing MemoryEngine, lifecycle state, zvec, and retrieval
```

The important provider decisions are:

1. Target the configured OpenCode provider for optional LLM extraction, but do
   not treat the current plugin client as implementation-ready. The current
   lockfile resolves OpenCode plugin/SDK 1.18.4, while the declared peer range
   permits later 1.x versions. The 1.18.4 root client does not expose the
   structured-output request/response fields needed by this plan. Phase 0 must
   pin and verify a compatible client surface or a deliberate adapter.
2. Do not use OpenCode as the embedding provider yet. Current OpenCode public
   APIs expose LLM/session prompting, not a generic `embed` or `embedMany`
   contract.
3. Keep the current local llama.cpp embedding default. It is already owned by
   the native daemon and protected by model identity, dimension, preprocessing,
   and collection fingerprints.
4. If remote embeddings are added later, integrate them through the current
   project model-switch architecture: immutable `EmbeddingProfileId` and
   `EmbeddingGeneration` values plus daemon-owned `ModelWorkerSupervisor` and
   `SwitchCoordinator` components. Do not add a second direct model owner inside
   `MemoryEngine`, and do not pretend that a chat model selected through OpenCode
   is an embedding model.
5. Keep graph extraction opt-in until content egress, cost, structured-output
   reliability, and extraction quality are measured on the project corpus.

## Review Decisions Requested

Please review these decisions before implementation:

| Decision                  | Proposed default                                                           | Why                                                                                      |
| ------------------------- | -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Graph ownership           | Actor-owned native project store under the existing writer lock            | Keeps one writer, crash recovery, project isolation, and local persistence authoritative |
| Graph scope               | Derived from source records plus explicit session/agent authorization keys | Prevents entity leakage and avoids trusting a plugin-supplied scope                      |
| Extraction provider       | OpenCode provider bridge after a compatibility spike                       | Reuses configured auth/model without making Rust depend on OpenCode internals            |
| Extraction default        | Disabled; explicit tool first                                              | Avoids silently sending documents to a hosted provider and avoids uncontrolled cost      |
| Embedding default         | Existing local Qwen3 GGUF through llama.cpp                                | Preserves privacy and current zvec compatibility                                         |
| Graph backend             | Native sidecar state first                                                 | Avoids adding Python, Neo4j, PostgreSQL, or an unmaintained embedded graph runtime       |
| First graph query         | Existing memory search seeds plus bounded graph expansion                  | Delivers value without replacing the tested hybrid retrieval path                        |
| Observation consolidation | Later phase                                                                | Derived summaries add LLM cost and can hide source facts if introduced too early         |

## Current Native Baseline

The repository already provides most of the non-LLM foundation needed for a
local knowledge base:

- `src/contract.rs::MemoryRecord` is the canonical API/domain projection. It is
  assembled from zvec content and lifecycle state rather than persisted as one
  object. It includes content, scope, origin, taxonomy, confidence, code
  anchors, lifecycle flags, and the existing `supersedes`, `superseded_by`, and
  `conflict_with` links.
- `src/storage/zvec.rs` stores dense vectors and lexical fields in one fixed
  embedding space per project collection.
- `src/storage/state.rs` persists lifecycle metadata, tombstones, feedback, and
  pending upsert/delete intents inside `state.json` with atomic file
  replacement. zvec mutation and document-manifest replacement are separately
  ordered operations recovered through those intents, not one filesystem
  transaction.
- `src/engine/mod.rs::MemoryEngine` is the project-owned mutation and retrieval
  boundary.
- `src/daemon/actor.rs` serializes project work and keeps the project actor as
  the storage writer.
- `src/embedding.rs::Embedder` currently exposes query and passage embedding;
  `LlamaCppEmbedder` is the only implementation.
- `src/document.rs` provides bounded explicit PDF, Markdown, and HTML ingestion.
  `src/document_index.rs` adds ignore-aware discovery and persists whole-file
  hash/chunk ownership in `document-index.json`. Explicit ingestion does not use
  that manifest and locates prior chunks through their source fields.
- `opencode-memory/src/plugin.ts` owns OpenCode hooks and tools, while
  `opencode-memory/src/daemon-client.ts` communicates with the native daemon
  over framed Protobuf on a private Unix socket.

The existing lifecycle relations must remain separate from graph facts:

```text
Lifecycle relation:
  memory A is replaced by memory B
  memory A and memory B are unresolved conflicting records

Knowledge relation:
  entity A caused, supports, contradicts, or is related to entity B
```

Putting entities and extracted predicates into `conflict_with` or `supersedes`
would make deletion, authorization, retrieval, and audit semantics ambiguous.
Graph `contradicts` must not be mapped onto the current symmetric
`conflict_with` relation, which has its own confidence and visibility rules.

## Research Findings

### Hindsight by Vectorize

The Hindsight documentation describes a write-time enrichment pipeline around a
memory bank:

1. Split retained content into chunks.
2. Use an LLM to extract atomic facts, entities, event times, causal
   relationships, and experience/world classification.
3. Resolve entities and create semantic, lexical, temporal, and graph indexes.
4. Consolidate raw facts into evidence-backed observations asynchronously.
5. Recall through several independent retrieval strategies, fuse rankings, and
   pack the result into a token budget.

Transferable ideas:

- Keep source provenance and the extracted fact separate.
- Store both event time and ingestion/learning time.
- Treat observations as derived data with source evidence, not as replacements
  for source facts.
- Make document ingestion idempotent with a stable document ID and source hash.
- Keep entity, semantic, temporal, and causal signals independently explainable.
- Use a dry-run extraction mode before persisting LLM output.
- Make invalidation auditable and reversible rather than hard-deleting every
  derived record immediately.

Important Hindsight constraints:

- A memory bank is the primary isolation boundary.
- The default and most complete documented storage path is PostgreSQL-specific,
  using pgvector, PostgreSQL full text search, JSONB, and recursive CTEs. Oracle
  Database 23ai is a documented alternative for core memory operations with
  operational limitations. Hindsight does not expose an arbitrary generic
  storage-provider abstraction.
- The docs describe local models through llama.cpp, Ollama, or LM Studio, but a
  remote LLM receives source chunks and context used for extraction.
- Some reviewed development defaults, particularly disabled authentication and
  open MCP exposure, require hardening for a private deployment. Binding,
  tracing, audit, and retention settings must be reviewed individually against
  the deployed Hindsight version rather than assumed from one bundled claim.
- Hindsight documentation is inconsistent about whether original source text is
  retained verbatim. However, its Documents, Recall, and Memory Defense pages
  explicitly describe persisted document bodies/chunks, `original_text`, and
  returning source chunks. Privacy analysis must therefore assume raw source
  persistence unless a specific retain mode is proven not to create it.

Hindsight is useful as a behavior reference, not as a storage dependency for
this project. Copying its PostgreSQL data model would discard the current
local-first actor, zvec, journal, and Markdown boundaries.

### Graphiti by Zep

Graphiti is a Python temporal context graph engine. Its core model is closer to
the requested knowledge base than a flat vector memory:

```text
Episode       raw source/provenance stream
Entity node   canonical person, project, concept, product, or other object
Entity edge   typed fact between two entities
Community     derived graph grouping and summary
```

Transferable ideas:

- Every derived fact traces back to an episode.
- Entity edges carry temporal validity such as `valid_at`, `invalid_at`,
  `expired_at`, `created_at`, and `reference_time`.
- Incremental ingestion can invalidate an old fact while preserving its
  historical record.
- Deduplication should be layered: exact normalization first, inexpensive fuzzy
  matching second, embedding similarity third, and LLM adjudication only for
  unresolved ambiguity.
- Retrieval can combine vector similarity, BM25/full text, graph traversal,
  node distance, MMR, and reciprocal rank fusion.
- LLMs, embedders, rerankers, graph drivers, and tracing have explicit client or
  provider abstractions. Search is separately configurable and delegates
  backend operations through the graph driver; it is not a peer `SearchClient`
  provider abstraction.

Important Graphiti constraints:

- Graphiti requires a supported graph backend. Neo4j is the default dependency;
  FalkorDB, deprecated Kuzu, and Neptune are optional paths. Additional search
  infrastructure is backend-specific: the Neptune path, for example, requires
  OpenSearch Serverless.
- Kuzu is marked deprecated in the current repository because upstream is no
  longer maintained. It should not be selected as the native storage direction.
- Ingestion can issue multiple LLM and embedding calls for extraction,
  deduplication, contradiction handling, and summaries. This is a cost and
  latency risk for document-heavy local memory.
- Structured output reliability varies across OpenAI-compatible providers,
  especially smaller or local models. Graphiti supports a configurable
  alternative from native JSON Schema mode to plain JSON mode with the schema
  included in the prompt; it is not documented here as an automatic retry
  fallback.
- Graphiti is Apache-2.0. Reusing its concepts does not require embedding the
  Python runtime, but copying implementation code would require preserving its
  license and notices.

Graphiti is therefore a good reference for temporal facts, provenance, and
provider seams, but its graph database and Python runtime are not the smallest
fit for this native daemon.

### OpenCode Provider Boundary

OpenCode plugins receive a client for the OpenCode server, but the structured
output surface is currently version-skewed:

- OpenCode v1.18.9 internal server schemas and prompt execution support
  `format: { type: "json_schema", schema }` and store an internal structured
  result.
- The v1.18.9 generated public SDK types reviewed during research omit the
  corresponding `format` request field and structured response field, while
  official SDK documentation uses a different `structured_output` name.
- This repository's current lockfile resolves plugin/SDK 1.18.4, while its
  declared peer range permits later 1.x versions. The 1.18.4 root
  `PluginInput.client` does not expose `format` or a structured response field.
  A separate V2 SDK surface exists in the installed package, but the plugin
  client is not typed as that V2 client.

Therefore structured extraction through the current typed plugin client is a
Phase 0 blocker, not an available API. The implementation must choose and test
one supported path: raise and pin the minimum OpenCode version, deliberately
construct/use a compatible V2 client, or use a weaker plain-text JSON parser.
The exact request and response field must come from the pinned generated types,
not from live documentation alone.

What this boundary gives us:

- A future provider adapter can follow the user's OpenCode configuration after
  the plugin defines how to obtain the provider/model identity. The current tool
  execution context does not directly provide it.
- Provider credentials remain owned by OpenCode's auth and provider services.
- The plugin does not need to read `auth.json` or copy API keys into native
  configuration.
- OpenCode-compatible local servers can also be configured as providers once the
  versioned adapter path is verified.

What this boundary does not give us:

- A generic public embeddings API. No stable `embed`/`embedMany` plugin API was
  found in the reviewed OpenCode version.
- A native-daemon callback path. Rust cannot assume that it can invoke the
  in-process OpenCode provider or access its credentials.
- A safe extraction session. Calling the current user session can create
  messages, trigger this plugin's own hooks, and introduce reentrancy or history
  pollution. An isolated extraction session or a proven non-history API is a
  Phase 0 prerequisite.
- A guaranteed structured-output contract for every provider/model. The native
  daemon still has to parse, bound, validate, and reject malformed candidates.
- A confirmed per-plugin provider budget or cost ceiling. Extraction therefore
  needs explicit batching, rate limits, and user-visible opt-in.

## Proposed Architecture

### Provider and process flow

The first implementation should make the plugin a narrow provider bridge. The
native daemon remains the authority for what may be persisted.

```text
User or explicit memory_graph_extract tool
  |
  v
TypeScript plugin
  1. request bounded extraction units from native
  2. apply remote-eligibility and redaction policy
  3. call the version-pinned OpenCode provider adapter in an isolated session
  4. validate response shape and size locally
  5. submit candidates with a durable run ID and extraction revisions to native
  |
  v
Native Rust daemon / ProjectActor-owned project store
  6. verify source IDs, hashes, revisions, authorization keys, and egress policy
  7. resolve entities deterministically
  8. assign native IDs and timestamps
  9. journal and persist graph state under the existing project writer lock
 10. update graph/index metadata and return accepted/rejected counts
```

The plugin must not directly write a graph file, open zvec, or resolve entity
IDs. Graph state must be a field of the actor-owned `MemoryEngine` or a new
actor-owned `ProjectStore` aggregate opened and recovered under the same project
writer lock. A separate graph writer would bypass the daemon's crash recovery.

The native daemon must not call an OpenCode provider by reaching into the
OpenCode auth store or assuming a server URL. That would couple a reusable
native binary to one host process and make credentials handling unclear.

### First API shape

The exact Protobuf tags are intentionally not selected in this document. The
logical operations are:

```text
graph_extract_prepare
  input: source memory IDs (including document-chunk memories), session scope
         key, agent scope key, and batch limits
  output: extraction units with source ID, per-memory content hash, record
          extraction revision, derived scope, bounded text, and
          remote-eligibility status

graph_upsert_candidates
  input: extraction run ID, session/agent authorization keys, source
         hashes/extraction revisions, provider/schema identity, normalized
         entities, typed relations, and evidence
  output: durable run receipt, accepted nodes/edges, rejected candidates,
          conflicts, and warnings

graph_run_status
  input: extraction run ID plus session/agent authorization keys
  output: persisted receipt or not-found result

graph_search
  input: query, session/agent authorization keys, time filters, and bounded
         depth/fanout/result limits
  output: ranked memories/entities/relations with provenance and score trace

graph_status
  input: session/agent authorization keys and optional scope filters
  output: graph schema version, node/edge counts, pending jobs, last extraction

graph_export
  input: session/agent authorization keys, scope filters, cursor, and page limit
  output: one bounded page of a vendor-neutral entity/relation/provenance archive
```

`graph_extract_prepare` is useful because the daemon can derive scope from the
verified source records, apply the caller's existing session/agent visibility
keys, and enforce content, secret, and egress policy before any content is sent
to a remote provider. Scope and remote eligibility must not be trusted from a
plugin-supplied label. The run binds each unit to a graph-specific extraction
revision: content hash plus scope, scope key, origin/source class, and egress
policy revision. It does not blindly use the general record revision because
pin, lock, feedback, and relation-only lifecycle changes can update metadata
without changing extractable content. This also lets a later local extractor use
the same contract.

The exact wire design is still open. Given the number of typed entities,
relations, evidence records, and run receipts, a new
`schema/opencode/memory/graph/v1/graph.proto` branch is safer than adding loose
objects to the generic memory payload. If graph methods remain in `memory.proto`,
they must start after its reserved model method range, increment domain schema
generation, advertise graph capabilities, and define TypeScript retry classes.
Exports must be paginated or file-oriented to stay below the current 32 MiB
response limit.

Regardless of the Protobuf file, graph requests are storage-owning project
requests. Actor routing must initialize and recover the actor-owned
`ProjectStore`/`MemoryEngine` under `writer.lock`; graph requests must not follow
the current model-control path that intentionally avoids engine initialization.

The first release should expose one explicit TypeScript tool such as
`memory_graph_extract`. Automatic document indexing must not silently invoke a
hosted provider. Phase 1 extraction is a foreground operation, but its final
upsert is idempotent by `extraction_run_id`: the same run ID with identical
extraction revisions and normalized candidates returns the existing receipt, while
the same ID with different material fails. `graph_run_status` resolves an
ambiguous transport outcome. A later durable job can let the plugin claim
pending extraction work, but the job state must be persisted in the native
daemon so plugin exit does not lose ownership.

The idempotency digest covers source IDs, content hashes, extraction revisions,
derived scope, policy revision, provider/model identity, extractor version,
prompt/schema versions, and the normalized candidate payload. A retained
non-sensitive receipt may keep that digest and counts. It must not keep evidence
quotes after deletion, and replaying a run whose source was deleted must return a
terminal source-deleted result rather than resurrecting facts.

### Extraction contract

The LLM should return candidates, not final database records. A minimal
provider-neutral shape is:

```json
{
  "entities": [
    {
      "mention": "zvec",
      "canonical_hint": "zvec",
      "entity_type": "technology",
      "aliases": [],
      "evidence": [{ "source_unit_id": "unit-1", "quote": "zvec" }],
      "confidence": 0.91
    }
  ],
  "relations": [
    {
      "subject_mention": "native memory",
      "predicate": "uses",
      "object_mention": "zvec",
      "relation_type": "technology_dependency",
      "valid_at": null,
      "invalid_at": null,
      "evidence": [{ "source_unit_id": "unit-1", "quote": "..." }],
      "confidence": 0.88
    }
  ]
}
```

The daemon, not the LLM, assigns canonical entity IDs, relation IDs, storage
timestamps, and scope keys. LLM-supplied IDs must be ignored.

Required extraction rules:

- The source is evidence, not instructions. Prompt injection-shaped text must
  never alter the extraction policy.
- An entity or relation without source evidence is rejected. Phase 1 does not
  persist unbounded raw provider output as a quarantine record.
- A relation must refer to entities in the same permitted graph scope.
- Confidence is a signal for review, not authorization to persist unsafe data.
- Temporal fields are optional. Missing time must not be invented.
- The output has hard limits on entity count, relation count, string length,
  evidence count, and total bytes.
- The extractor version, provider, model, prompt/schema hash, source hashes, and
  accepted normalized candidate payload are recorded for idempotency, index
  rebuild, and quality comparison, but raw credentials are not.
- Evidence uses an exact quote plus occurrence metadata. If offsets are added,
  the contract must define one cross-language unit, preferably UTF-8 bytes;
  JavaScript UTF-16 indexes must never be interpreted as Rust byte offsets.
- The daemon rechecks source ID, content hash, extraction revision, scope,
  visibility, and egress-policy revision immediately before commit.

### Native graph model

The first graph model should reuse existing memories as source episodes. It does
not need a duplicate `Fact` object for every memory chunk.

```text
GraphScope
  project ID + memory scope + verified scope key

SourcePolicy
  origin/policy revision + remote egress decision + deletion policy

GraphEntity
  native ID
  canonical name
  entity type
  aliases
  first_seen / last_seen
  scope and provenance counters

GraphMention
  source memory ID
  entity ID
  evidence quote/occurrence

GraphRelation
  native ID
  subject entity ID
  predicate / relation type
  object entity ID
  valid_at / invalid_at
  created_at / extracted_at
  confidence and status
  source memory IDs and evidence spans
  extractor version

GraphObservation (later)
  derived statement or summary
  supporting relation IDs and source memory IDs
  proof count and freshness
  contradiction/history references
```

The existing `MemoryRecord` plays two roles in this design:

- source episode/evidence for the graph;
- searchable text/vector record for the current memory engine.

This avoids copying source content into every graph node. Graph currentness is
not inferred from a real-world `invalid_at` timestamp when a source memory is
deleted. A relation is query-eligible only while at least one visible,
non-pending, non-expired, non-disallowed-stale, non-superseded source evidence
record still has the extraction revision bound to that evidence. Graph cleanup
is derived maintenance and must be called from every source mutation path:
store overwrite, capture replacement, update, delete/forget, expiry
optimization, explicit ingest replacement, document index replacement/removal,
shared Markdown sync, import, and purge.

Deletion policy must be explicit:

- `user_deleted` and project purge remove graph evidence quotes and private
  derived content, rather than keeping a hidden copy marked inactive;
- obsolete or superseded evidence may retain bounded historical provenance only
  when the existing delete policy permits it;
- the current tombstone contains a fingerprint, not the source memory ID or
  source content, so cleanup must happen before source deletion or via a
  durable source-to-graph ownership record.

User deletion and purge cannot report success while a queryable hidden graph
copy remains. Before source removal, the actor must durably commit graph erasure
or persist an erasure intent that immediately makes the graph payload
ineligible and guarantees physical erasure during restart recovery. A completed
delete response requires the graph erasure itself to be durable; the intent is
for crash recovery, not indefinite retention.

Suggested first relation types for entity-to-entity edges:

```text
uses
depends_on
implements
causes
related_to
```

`mentions` is a `GraphMention` association, not an entity-to-entity edge.
`supports` and `contradicts` should be added only after introducing a real
assertion/fact type or relation-to-relation references. `supersedes` is also
intentionally deferred until graph fact invalidation is clearly separated from
memory record lifecycle supersession.

### Entity resolution

Use the Graphiti-inspired cost order, but keep the first version deterministic:

1. Normalize a specified Unicode form, case, and whitespace and compare exact
   aliases within the same graph scope.
2. Apply bounded token or fuzzy matching only when entity type and scope agree.
3. Use local embedding similarity only if a separate, benchmarked entity index
   exists. Do not compare arbitrary model spaces.
4. Defer LLM adjudication to an explicit review mode. It is too expensive and
   nondeterministic for the default write path.

An entity key must include the graph scope. The string `backend` in one project
must not automatically resolve to the entity `backend` in another project or
to a personal/session entity with a different policy. The normalization and
fuzzy algorithm versions must be persisted because deterministic rebuilds depend
on them. The implementation must explicitly choose a Unicode-normalization
dependency or a documented ASCII/Unicode policy; Rust's standard library alone
does not provide canonical Unicode normalization.

### Storage layout

Phase 1 should use a graph sidecar owned by the actor's project store. The
actual path is under the configured data root, not the repository checkout:

```text
<data-root>/projects/<project-id>/
  writer.lock                 # existing project ownership
  manifest.json               # existing vector manifest
  state.json                  # existing lifecycle state
  zvec/                       # existing memory search collection
  document-index.json         # existing source/chunk ownership
  knowledge-graph.json        # accepted facts/evidence/runs plus graph state
  knowledge-graph.pending.json # one bounded idempotent graph transaction
```

The graph sidecar must have its own schema version, manifest, source-eligibility
policy version, and extraction algorithm version. Accepted normalized entities,
relations, evidence, and extraction run payloads are canonical graph facts.
Adjacency maps and retrieval indexes are rebuildable projections. Losing those
facts means re-extraction requires the provider; hashes and prompt metadata alone
are not enough to rebuild the result.

A graph write should follow a graph-specific durable pattern while acknowledging
that it is not one atomic filesystem transaction with zvec and `state.json`:

```text
validate candidates
  -> write pending graph transaction with run ID
  -> atomically replace graph state/index projection
  -> fsync parent where supported
  -> clear pending graph transaction
```

The graph is still derived from memory content and must never become a second
source of truth for memory text. To reconcile separate source and graph stores,
graph reads must verify current source eligibility; stale graph evidence may be
cleaned asynchronously without making ordinary memory retrieval unavailable.

If graph size later makes a single JSON sidecar too expensive, the next storage
step should be a native embedded transactional store selected by benchmark and
crash-recovery tests. It should not be an unplanned switch to PostgreSQL or a
Python Graphiti subprocess.

### Retrieval and ranking

Do not replace the current memory search in phase 1. Add graph traversal as a
separate ranked channel:

```text
query
  -> existing lexical/dense/hybrid memory search
  -> graph entity/relation seed lookup
  -> bounded BFS/typed expansion, depth 1-2
  -> lifecycle/scope/trust/time filters
  -> rank fusion by RRF for the new graph channel
  -> optional reranking later
  -> existing context-budget packing (`budget_chars`), not an assumed token API
```

The graph channel should return source memory IDs and relation evidence, not
only a generated summary. Every result must remain inspectable through
`memory_get` or a future graph detail operation.

The existing retrieval pipeline is not itself pure RRF: it combines dense
similarity, lexical and reciprocal-rank signals, taxonomy weighting, logistic
calibration, retention, feedback, MMR, and a character budget. RRF here is a new
cross-channel fusion proposal. Phase 1 should preferably project graph results
back to eligible source memory IDs; heterogeneous entity/relation result objects
need a separate response contract and packing policy.

Do not combine raw cosine scores from different embedding models. If a future
entity index uses another model, retrieve within that model generation and fuse
ranked lists, following the existing project embedding model-switch plan.

Initial graph query limits should be conservative:

- maximum traversal depth: 2;
- maximum outgoing edges per node: 32;
- maximum returned graph facts: 64;
- maximum evidence memories per fact: 8;
- maximum graph result characters: the existing memory context budget.

These values are starting points for tests, not public compatibility promises.

## Provider Decision: LLM Extraction vs Embedding

| Capability                | OpenCode provider                                               | Current native local path              | Recommendation                                            |
| ------------------------- | --------------------------------------------------------------- | -------------------------------------- | --------------------------------------------------------- |
| Structured LLM extraction | Internal support exists, current plugin SDK blocked             | Not implemented as an LLM extractor    | Resolve and pin the client surface in Phase 0             |
| Text embedding            | No public stable API found in reviewed v1.18.4/v1.18.9 surfaces | `LlamaCppEmbedder` with pinned GGUF    | Keep local default                                        |
| Image embedding           | No public stable API found in reviewed v1.18.4/v1.18.9 surfaces | Not in current implementation          | Defer until multi-space model work                        |
| Reranking                 | No generic plugin contract confirmed                            | Existing retrieval scoring             | Defer; do not add provider dependency yet                 |
| Credentials               | OpenCode service-owned, plugin process not sandboxed            | Native Hugging Face/local model config | Never copy OpenCode credentials into native config        |
| Privacy                   | Extraction provider may receive complete source units           | Embedding inference is local           | Distinguish extraction egress from existing recall egress |

Graph extraction and embedding are different contracts:

```text
LLM extraction:
  text -> structured entities, predicates, evidence, time hints

Embedding:
  text -> fixed-dimensional numeric vector
```

A configured OpenCode chat model cannot be treated as an embedding model merely
because both are called "models". The vector dimension, metric, preprocessing,
query/passage behavior, and artifact identity must be known and included in the
project embedding-space identity.

The existing `src/embedding.rs::Embedder` trait is only a small current
engine-facing interface, not a ready remote-provider seam: it is crate-private,
`MemoryEngine` stores concrete `LlamaCppEmbedder`, and it has no batching,
cancellation, metric, or embedding-generation identity. Future backends should
align with the existing project model-switch plan's `EmbeddingProfileId`,
`EmbeddingGeneration`, `ModelWorkerSupervisor`, and `SwitchCoordinator`. A
remote implementation would still need:

- provider endpoint and model identity;
- dimension and metric discovery or explicit configuration;
- batching, timeout, retry, cancellation, and rate-limit policy;
- content-egress policy by memory scope;
- configuration fingerprint and collection migration;
- behavior when the provider is unavailable;
- secret handling outside the OpenCode auth store.

That is a separate feature from graph extraction and should not be bundled into
the first graph implementation.

## Security and Privacy Requirements

Embedding inference is currently local. However, automatic recall can already
place selected memory excerpts into the OpenCode chat prompt, which may be sent
to a hosted chat provider. Graph extraction introduces a different and larger
egress purpose: it can send complete source units in bulk and persist derived
facts. The following must be true before enabling remote extraction:

1. The user explicitly enables the provider mode or invokes the explicit graph
   extraction tool.
2. The native prepare step marks whether each source unit may be sent remotely.
3. Secret-like and credential-like content is blocked or redacted before the
   provider request, not only after graph persistence.
4. Prompt injection-shaped source text is placed in a clearly delimited evidence
   section and cannot change extraction instructions.
5. Provider/model IDs, schema hash, source hashes, byte counts, and token usage
   are recorded without logging raw source text or credentials.
6. Batches, request timeouts, retries, and total extraction work are bounded.
7. A provider failure leaves the existing memory and graph unchanged.
8. A malformed or unsupported response is rejected rather than retried without
   a limit or persisted as raw provider output.
9. Entity resolution never crosses project, agent, session, or repository trust
   boundaries without an explicit policy.
10. Graph export makes source provenance and derived status visible so users can
    delete or rebuild derived data.
11. Session and agent authorization keys are present on prepare, upsert, search,
    status, receipt, and export operations; scope is derived from source records.
12. `user_deleted` and purge operations remove graph quotes and private derived
    content before the source record disappears.
13. Extraction uses an isolated session or a proven non-history API with tools
    disabled and this plugin's normal recall/capture hooks suppressed.

The OpenCode plugin runs in the OpenCode server process and its client is not a
separate security sandbox. Provider selection and extraction must therefore be
treated as an application-level privacy decision, not as a native-daemon
isolation guarantee.

## Alternatives Rejected for the First Phase

### Embed Graphiti directly

Rejected for the first phase. It would add a Python runtime, graph-driver
dependency, separate persistence ownership, and an additional service boundary.
Its temporal and provenance concepts should be adapted into Rust-native value
types instead.

### Adopt Hindsight's PostgreSQL storage

Rejected for the first phase. Hindsight's default and most complete storage path
is PostgreSQL-specific; Oracle 23ai is a documented but operationally limited
alternative, and no arbitrary storage-provider interface is exposed. The native
daemon already has a local zvec/state/journal design and does not need a second
database.

### Put the graph in `MemoryRecord` lifecycle relations

Rejected. Lifecycle supersession/conflict is not the same domain as entity
predicates, temporal facts, or evidence links. Mixing them would weaken current
validation invariants.

### Call the OpenCode provider from Rust

Rejected. The provider and credentials are owned by the OpenCode process. A Rust
daemon should not read OpenCode auth files or depend on an undocumented server
socket/URL. The TypeScript plugin is the explicit adapter boundary.

### Replace local embeddings with an OpenCode chat model

Rejected. No generic embedding contract is currently exposed, and chat output
is not a vector space. Replacing the current model would also require explicit
collection migration and retrieval benchmarks.

### Run LLM extraction automatically for every indexed document

Rejected initially. It creates hidden content egress, unbounded model cost,
provider rate-limit failures, and a background job ownership problem when the
plugin disconnects. Start with explicit extraction and add a durable queue only
after the contract is proven.

## Rollout Proposal

### Phase 0: Provider and extraction spike

Deliverables:

- an explicit decision for the OpenCode client surface: supported root client,
  deliberate V2 client, or plain-text JSON fallback;
- one version-pinned provider call with its real generated request and response
  types;
- extraction JSON Schema and fallback parser;
- isolated extraction-session lifecycle with tools disabled and normal memory
  hooks suppressed;
- a tested source for current provider/model identity;
- English/Vietnamese/code fixtures with expected entities and relations;
- provider failure, malformed JSON, timeout, and cancellation tests;
- redaction and remote-eligibility check before provider dispatch;
- cost, latency, and output-size measurements;
- no graph persistence.

Exit criteria:

- the configured provider can return a bounded schema reliably;
- the current 1.18.4 type mismatch is either removed by a supported minimum
  version or handled by a deliberate tested adapter;
- the exact SDK response field is verified for the supported OpenCode version;
- extraction creates no messages in the user's active conversation and cannot
  recursively invoke memory hooks;
- malformed output never reaches native graph persistence;
- explicit provider opt-in is observable to the user;
- local provider and hosted provider behavior are documented separately.

### Phase 1: Native graph sidecar and explicit tool

Deliverables:

- Rust graph value types and validation;
- graph sidecar manifest and pending journal;
- prepare/upsert/run-status/search/status/export domain contracts with explicit
  session and agent authorization keys;
- TypeScript provider bridge and `memory_graph_extract` tool;
- deterministic entity resolution within scope;
- source hash/revision, evidence, egress-policy, and deletion validation;
- idempotent extraction-run receipt and outcome reconciliation;
- persisted accepted normalized candidate payload for index rebuild;
- graph status and diagnostics.

Exit criteria:

- one project actor remains the only graph writer;
- a daemon restart recovers or discards pending graph writes safely;
- graph queries require at least one current visible source evidence record;
- user deletion and purge remove hidden graph copies according to policy;
- adjacency/search indexes rebuild from accepted normalized graph facts without
  provider credentials; re-extracting lost facts still requires a provider;
- retrying an identical run ID returns its existing receipt, and changing the
  run material under the same ID fails;
- extraction is never triggered by ordinary document indexing unless explicitly
  enabled.

### Phase 2: Temporal facts and graph retrieval

Deliverables:

- `valid_at`/`invalid_at` query semantics;
- contradiction and invalidation history;
- graph-seed retrieval and bounded expansion;
- RRF score trace and context-budget integration;
- graph benchmark with no-answer and stale-fact cases;
- optional reviewed observations backed by source evidence.

Exit criteria:

- historical queries do not return invalid facts as current facts;
- every graph result can show source evidence;
- graph expansion does not bypass scope, lifecycle, or safety/eligibility
  filters;
- graph retrieval improves the frozen benchmark without regressing existing
  lexical/dense/hybrid retrieval.

### Phase 3: Durable background extraction

Only after the explicit tool is reliable:

- persist extraction jobs and extraction revisions in the native daemon;
- let a plugin worker claim and renew only an expiring provider-work lease;
- keep the durable graph job, internal project job lease, receipt, retry, and
  source-mutation ownership in the daemon; reuse the coordinator/job-lease
  pattern proposed by the model-switch plan rather than creating a graph-only
  scheduler;
- resume after plugin or daemon restart;
- keep remote extraction disabled for scopes that do not permit egress.

### Phase 4: Optional embedding backends

This is independent of the graph sidecar and requires a separate review of the
current project embedding model-switch plan:

- evaluate a remote provider adapter behind `ModelWorkerSupervisor` or a native
  worker, not directly inside `MemoryEngine`;
- introduce an explicit immutable embedding profile and generation migration;
- store incompatible vectors in separate generations/collections;
- benchmark local vs remote quality, cost, latency, and privacy;
- when a provider is unavailable, degrade to lexical retrieval or a separately
  indexed compatible space. Never substitute a different local model into an
  existing remote model's vector space.

## Acceptance Criteria

The knowledge graph feature should not be considered complete until:

1. The current memory APIs and local embedding behavior remain compatible.
2. Existing zvec collections open without graph extraction or model migration.
3. Graph writes are journaled and recoverable through the project actor.
4. Graph indexes can be rebuilt from persisted accepted normalized facts,
   provenance, and extraction runs. Source-only rebuild is explicitly a new
   extraction operation.
5. Entity and relation IDs are assigned natively, never trusted from the LLM.
6. Graph relations remain separate from lifecycle supersession/conflict links.
7. Every active relation has at least one current visible source evidence record
   and a scope derived from that source.
8. Remote provider use is explicit, bounded, and visible.
9. Provider credentials never enter native configuration or graph state.
10. Malformed, unsafe, stale, or out-of-scope candidates are rejected without
    persisting raw provider output or mutating active graph state.
11. Graph retrieval uses rank fusion and never compares incompatible raw vector
    scores.
12. Current memory retrieval remains useful when the provider is unavailable.
13. Temporal invalidation preserves historical provenance.
14. Tests cover duplicate ingestion, entity collision, contradiction, deletion,
    scope isolation, restart recovery, provider timeout, and no-answer queries.

## Open Questions

Questions 1, 2, 6, and 7 are Phase 0 prerequisites, not optional follow-up
decisions. All questions must be decided before the Protobuf contract is frozen:

1. Should graph extraction be allowed for repository-scoped memories, or only
   private project/agent memory?
2. Should hosted extraction be allowed for source code by default, or require a
   separate permission from general document extraction?
3. Is the first graph query expected to be a new `memory_graph_search` tool, or
   should `memory_search` gain an opt-in `include_graph` field?
4. Should observations be part of phase 1, or should the first release expose
   only source-backed entities and relations?
5. What is the acceptable maximum graph sidecar size before a transactional
   embedded store is required?
6. Which isolated extraction-session or non-history provider API will be used,
   and how will provider/model identity, hook suppression, cleanup, and tool
   disabling work?
7. Which provider/model should be the first benchmark target, and what is the
   fallback behavior when its structured output is unreliable?

## Sources

All external research was checked on 2026-07-29. Upstream behavior can change;
implementation should pin versions and verify the exact SDK/API surface.

### Hindsight

- [Hindsight storage](https://hindsight.vectorize.io/developer/storage)
- [Hindsight retain](https://hindsight.vectorize.io/developer/retain)
- [Hindsight retain API](https://hindsight.vectorize.io/developer/api/retain)
- [Hindsight observations](https://hindsight.vectorize.io/developer/observations)
- [Hindsight retrieval](https://hindsight.vectorize.io/developer/retrieval)
- [Hindsight memory and document APIs](https://hindsight.vectorize.io/developer/api/memories)
- [Hindsight document API](https://hindsight.vectorize.io/developer/api/documents)
- [Hindsight configuration and models](https://hindsight.vectorize.io/developer/configuration)
- [Hindsight model providers](https://hindsight.vectorize.io/developer/models)
- [Hindsight memory defense](https://hindsight.vectorize.io/developer/memory-defense)
- [Hindsight MCP server](https://hindsight.vectorize.io/developer/mcp-server)
- [Hindsight OpenAPI 0.8.5](https://hindsight.vectorize.io/openapi.json)
- [Hindsight v0.8.5 release](https://github.com/vectorize-io/hindsight/releases/tag/v0.8.5)
- [Hindsight repository license](https://github.com/vectorize-io/hindsight/blob/v0.8.5/LICENSE)
- [Hindsight Oracle backend](https://hindsight.vectorize.io/developer/oracle)

### Graphiti

- [Graphiti v0.29.3 source snapshot](https://github.com/getzep/graphiti/tree/00b0130bab4544574deb4ea8b1d30ceb82de9c5c)
- [Graphiti README and context graph model](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/README.md)
- [`Graphiti` orchestration](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/graphiti.py)
- [Graphiti graph types](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/graphiti_types.py)
- [LLM client abstraction](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/llm_client/client.py)
- [OpenAI-compatible LLM client](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/llm_client/openai_generic_client.py)
- [Embedding client abstraction](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/embedder/client.py)
- [Graph driver abstraction](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/driver/driver.py)
- [Hybrid search implementation](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/search/search.py)
- [Search configuration recipes](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/graphiti_core/search/search_config_recipes.py)
- [Graphiti Apache-2.0 license](https://github.com/getzep/graphiti/blob/00b0130bab4544574deb4ea8b1d30ceb82de9c5c/LICENSE)

### OpenCode

- [OpenCode plugins](https://opencode.ai/docs/plugins/)
- [OpenCode SDK](https://opencode.ai/docs/sdk/)
- [OpenCode providers](https://opencode.ai/docs/providers/)
- [OpenCode permissions](https://opencode.ai/docs/permissions/)
- [OpenCode plugin API source, v1.18.9](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/plugin/src/index.ts)
- [OpenCode prompt execution, v1.18.9](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/opencode/src/session/prompt.ts)
- [OpenCode session schema, v1.18.9](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/schema/src/v1/session.ts)
- [OpenCode generated SDK types, v1.18.9](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/sdk/js/src/gen/types.gen.ts)
- [OpenCode LLM design and embedding scope, v1.18.9](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/llm/DESIGN.md)
- [OpenCode release baseline, v1.18.9](https://github.com/anomalyco/opencode/releases/tag/v1.18.9)

### Local repository references

- [Package manifest](../package.json)
- [Resolved dependency lock](../bun.lock)
- [`MemoryRecord`](../src/contract.rs)
- [Native embedding abstraction](../src/embedding.rs)
- [Memory engine](../src/engine/mod.rs)
- [Lifecycle state](../src/storage/state.rs)
- [zvec storage](../src/storage/zvec.rs)
- [Document ingestion](../src/document.rs)
- [Document index](../src/document_index.rs)
- [Configuration and embedding fingerprint](../src/config.rs)
- [Project actor](../src/daemon/actor.rs)
- [Retrieval scoring](../src/engine/retrieval.rs)
- [Memory protocol](../schema/opencode/memory/v1/memory.proto)
- [Daemon protocol](../schema/opencode/memory/daemon/v1/daemon.proto)
- [TypeScript plugin](../opencode-memory/src/plugin.ts)
- [Daemon client](../opencode-memory/src/daemon-client.ts)
- [TypeScript protocol mapping](../opencode-memory/src/protocol.ts)
- [Plugin-local background jobs](../opencode-memory/src/background-jobs.ts)
- [Project embedding model switch plan](./project-embedding-model-switch-plan.md)
