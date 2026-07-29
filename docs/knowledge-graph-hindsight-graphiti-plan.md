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

1. Reuse the configured OpenCode provider for optional LLM extraction. The
   plugin can call the OpenCode session prompt API with structured JSON output.
2. Do not use OpenCode as the embedding provider yet. Current OpenCode public
   APIs expose LLM/session prompting, not a generic `embed` or `embedMany`
   contract.
3. Keep the current local llama.cpp embedding default. It is already owned by
   the native daemon and protected by model identity, dimension, preprocessing,
   and collection fingerprints.
4. If remote embeddings are added later, add a direct embedding backend or a
   native model broker. Do not pretend that a chat model selected through
   OpenCode is an embedding model.
5. Keep graph extraction opt-in until content egress, cost, structured-output
   reliability, and extraction quality are measured on the project corpus.

## Review Decisions Requested

Please review these decisions before implementation:

| Decision                  | Proposed default                                          | Why                                                                                      |
| ------------------------- | --------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Graph ownership           | Native Rust daemon                                        | Keeps one writer, crash recovery, project isolation, and local persistence authoritative |
| Graph scope               | Same project and scope boundaries as memory               | Prevents entity leakage between projects, agents, and sessions                           |
| Extraction provider       | OpenCode provider through the TypeScript plugin           | Reuses configured auth/model without making Rust depend on OpenCode internals            |
| Extraction default        | Disabled; explicit tool first                             | Avoids silently sending documents to a hosted provider and avoids uncontrolled cost      |
| Embedding default         | Existing local Qwen3 GGUF through llama.cpp               | Preserves privacy and current zvec compatibility                                         |
| Graph backend             | Native sidecar state first                                | Avoids adding Python, Neo4j, PostgreSQL, or an unmaintained embedded graph runtime       |
| First graph query         | Existing memory search seeds plus bounded graph expansion | Delivers value without replacing the tested hybrid retrieval path                        |
| Observation consolidation | Later phase                                               | Derived summaries add LLM cost and can hide source facts if introduced too early         |

## Current Native Baseline

The repository already provides most of the non-LLM foundation needed for a
local knowledge base:

- `src/contract.rs::MemoryRecord` is the canonical memory record. It includes
  content, scope, origin, taxonomy, confidence, code anchors, lifecycle flags,
  and the existing `supersedes`, `superseded_by`, and `conflict_with` links.
- `src/storage/zvec.rs` stores dense vectors and lexical fields in one fixed
  embedding space per project collection.
- `src/storage/state.rs` persists lifecycle metadata, tombstones, feedback,
  and pending mutation journals with atomic file replacement.
- `src/engine/mod.rs::MemoryEngine` is the project-owned mutation and retrieval
  boundary.
- `src/daemon/actor.rs` serializes project work and keeps the project actor as
  the storage writer.
- `src/embedding.rs::Embedder` currently exposes query and passage embedding;
  `LlamaCppEmbedder` is the only implementation.
- `src/document.rs` and `src/document_index.rs` already provide bounded,
  gitignore-aware PDF, Markdown, and HTML ingestion with source hashes and
  chunk ownership.
- `opencode-memory/src/plugin.ts` owns OpenCode hooks and tools, while
  `opencode-memory/src/daemon-client.ts` communicates with the native daemon
  over framed Protobuf on a private Unix socket.

The existing lifecycle relations must remain separate from graph facts:

```text
Lifecycle relation:
  memory A is replaced by memory B

Knowledge relation:
  entity A caused, supports, contradicts, or is related to entity B
```

Putting entities and extracted predicates into `conflict_with` or
`supersedes` would make deletion, authorization, retrieval, and audit semantics
ambiguous.

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
- The documented storage implementation is PostgreSQL-oriented, using pgvector,
  PostgreSQL full text search, JSONB, and recursive CTEs. The storage page says
  there is no generic storage abstraction.
- The docs describe local models through llama.cpp, Ollama, or LM Studio, but a
  remote LLM receives source chunks and context used for extraction.
- Authentication, API binding, MCP access, tracing, and audit defaults need
  hardening for a private deployment. The reviewed documentation describes
  insecure-by-default development settings in some areas.
- Hindsight documentation is inconsistent about whether original source text is
  retained verbatim. The conservative assumption is that source chunks and
  extracted text can remain persisted unless the exact deployment proves
  otherwise.

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
- LLM, embedding, graph storage, and search components are separate provider
  abstractions in the core design.

Important Graphiti constraints:

- The normal deployment expects a graph backend such as Neo4j, FalkorDB, Kuzu,
  or Neptune plus supporting search infrastructure.
- Kuzu is marked deprecated in the current repository because upstream is no
  longer maintained. It should not be selected as the native storage direction.
- Ingestion can issue multiple LLM and embedding calls for extraction,
  deduplication, contradiction handling, and summaries. This is a cost and
  latency risk for document-heavy local memory.
- Structured output reliability varies across OpenAI-compatible providers,
  especially smaller or local models. Graphiti supports a fallback from native
  JSON Schema mode to plain JSON mode with the schema included in the prompt.
- Graphiti is Apache-2.0. Reusing its concepts does not require embedding the
  Python runtime, but copying implementation code would require preserving its
  license and notices.

Graphiti is therefore a good reference for temporal facts, provenance, and
provider seams, but its graph database and Python runtime are not the smallest
fit for this native daemon.

### OpenCode Provider Boundary

OpenCode plugins receive a client for the OpenCode server. Current official
plugin/SDK documentation shows structured session prompting with a selected
`providerID`, `modelID`, and a `json_schema` response format. The relevant shape
is conceptually:

```ts
const result = await client.session.prompt({
  path: { id: sessionId },
  body: {
    model: { providerID, modelID },
    parts: [{ type: "text", text: extractionPrompt }],
    format: {
      type: "json_schema",
      schema: extractionSchema,
    },
  },
});

const extracted = result.data.info.structured_output;
```

This example is intentionally illustrative. OpenCode has had more than one SDK
generation, and some SDK documentation exposes `session.chat` while current
plugin documentation exposes `session.prompt`. The implementation must pin the
OpenCode version and verify the exact installed type and response field before
adding code.

What this boundary gives us:

- Provider/model selection can follow the user's OpenCode configuration.
- Provider credentials remain owned by OpenCode's auth and provider services.
- The plugin does not need to read `auth.json` or copy API keys into native
  configuration.
- OpenCode-compatible local servers can also be selected as providers.

What this boundary does not give us:

- A generic public embeddings API. No stable `embed`/`embedMany` plugin API was
  found in the reviewed OpenCode version.
- A native-daemon callback path. Rust cannot assume that it can invoke the
  in-process OpenCode provider or access its credentials.
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
  3. call OpenCode provider with JSON Schema output
  4. validate response shape and size locally
  5. submit candidates with source hashes to native
  |
  v
Native Rust daemon / ProjectActor
  6. verify source IDs, hashes, scope, limits, and trust policy
  7. resolve entities deterministically
  8. assign native IDs and timestamps
  9. journal and atomically persist graph state
 10. update graph/index metadata and return accepted/rejected counts
```

The plugin must not directly write a graph file, open zvec, or resolve entity
IDs. That would create a second writer and bypass the daemon's crash recovery.

The native daemon must not call an OpenCode provider by reaching into the
OpenCode auth store or assuming a server URL. That would couple a reusable
native binary to one host process and make credentials handling unclear.

### First API shape

The exact Protobuf tags are intentionally not selected in this document. The
logical operations are:

```text
graph_extract_prepare
  input: source memory IDs or document chunk IDs, scope, batch limits
  output: extraction units with source IDs, content hash, bounded text, and
          remote-eligibility status

graph_upsert_candidates
  input: extraction run ID, source hashes, entities, typed relations, evidence
  output: accepted nodes/edges, quarantined candidates, conflicts, warnings

graph_search
  input: query, scope, time filters, depth/fanout/token limits
  output: ranked memories/entities/relations with provenance and score trace

graph_status
  input: none or scope
  output: graph schema version, node/edge counts, pending jobs, last extraction

graph_export
  input: scope and inclusion flags
  output: vendor-neutral source/fact/entity/relation/provenance archive
```

`graph_extract_prepare` is useful because the daemon can enforce source scope,
content limits, secret policy, and a source hash before any content is sent to a
remote provider. It also lets a later local extractor use the same extraction
contract.

The first release should expose one explicit TypeScript tool such as
`memory_graph_extract`. Automatic document indexing must not silently invoke a
hosted provider. A later durable job can let the plugin claim pending extraction
work, but the job state must be persisted in the native daemon so plugin exit
does not lose ownership or leave an ambiguous mutation.

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
      "evidence": [{ "source_unit_id": "unit-1", "start": 12, "end": 16 }],
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
- An entity or relation without source evidence is rejected or quarantined.
- A relation must refer to entities in the same permitted graph scope.
- Confidence is a signal for review, not authorization to persist unsafe data.
- Temporal fields are optional. Missing time must not be invented.
- The output has hard limits on entity count, relation count, string length,
  evidence count, and total bytes.
- The extractor version, provider, model, prompt/schema hash, and source hashes
  are recorded for replay and quality comparison, but raw credentials are not.

### Native graph model

The first graph model should reuse existing memories as source episodes. It does
not need a duplicate `Fact` object for every memory chunk.

```text
GraphScope
  project ID + memory scope + scope key + trust boundary

GraphEntity
  native ID
  canonical name
  entity type
  aliases
  first_seen / last_seen
  scope and provenance counters

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
  supporting relation/fact IDs
  proof count and freshness
  contradiction/history references
```

The existing `MemoryRecord` plays two roles in this design:

- source episode/evidence for the graph;
- searchable text/vector record for the current memory engine.

This avoids copying source content into every graph node and keeps graph
deletion tied to existing memory lifecycle events. If a memory is tombstoned,
its graph evidence becomes inactive or quarantined according to policy; the
graph does not silently keep an unsupported fact as current truth.

Suggested first relation types:

```text
mentions
uses
depends_on
implements
causes
supports
contradicts
related_to
```

`supersedes` is intentionally not in this first list. It can be added only
after deciding how a graph fact's temporal invalidation differs from a memory
record's lifecycle supersession.

### Entity resolution

Use the Graphiti-inspired cost order, but keep the first version deterministic:

1. Normalize Unicode/case/whitespace and compare exact aliases within the same
   graph scope.
2. Apply bounded token or fuzzy matching only when entity type and scope agree.
3. Use local embedding similarity only if a separate, benchmarked entity index
   exists. Do not compare arbitrary model spaces.
4. Defer LLM adjudication to an explicit review mode. It is too expensive and
   nondeterministic for the default write path.

An entity key must include the graph scope. The string `backend` in one project
must not automatically resolve to the entity `backend` in another project or
to a personal/session entity with a different trust boundary.

### Storage layout

Phase 1 should use a graph sidecar owned by the project actor:

```text
projects/<project-id>/
  state.json                  # existing lifecycle state
  zvec/                       # existing memory search collection
  document-index.json         # existing source/chunk ownership
  knowledge-graph.json        # graph nodes, relations, evidence, manifest
  knowledge-graph.pending.json
```

The graph sidecar must have its own schema version and manifest. A graph write
should follow the existing durable pattern:

```text
validate candidates
  -> write pending graph journal
  -> atomically replace graph state
  -> fsync parent where supported
  -> clear pending journal
```

The graph sidecar is a canonical derived index, not a second source of truth for
memory content. It must be exportable and rebuildable from source memories plus
extraction metadata.

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
  -> rank fusion by RRF
  -> optional reranking later
  -> existing context-budget packing
```

The graph channel should return source memory IDs and relation evidence, not
only a generated summary. Every result must remain inspectable through
`memory_get` or a future graph detail operation.

Do not combine raw cosine scores from different embedding models. If a future
entity index uses another model, retrieve within that space and fuse ranked
lists, following the existing multi-model embedding plan.

Initial graph query limits should be conservative:

- maximum traversal depth: 2;
- maximum outgoing edges per node: 32;
- maximum returned graph facts: 64;
- maximum evidence memories per fact: 8;
- maximum graph result characters: the existing memory context budget.

These values are starting points for tests, not public compatibility promises.

## Provider Decision: LLM Extraction vs Embedding

| Capability                | OpenCode provider                      | Current native local path              | Recommendation                                        |
| ------------------------- | -------------------------------------- | -------------------------------------- | ----------------------------------------------------- |
| Structured LLM extraction | Feasible through plugin session prompt | Not implemented as an LLM extractor    | Use OpenCode provider, opt-in, with native validation |
| Text embedding            | No generic stable OpenCode API found   | `LlamaCppEmbedder` with pinned GGUF    | Keep local default                                    |
| Image embedding           | No generic stable OpenCode API found   | Not in current implementation          | Defer until multi-space model work                    |
| Reranking                 | No generic plugin contract confirmed   | Existing retrieval scoring             | Defer; do not add provider dependency yet             |
| Credentials               | OpenCode-owned                         | Native Hugging Face/local model config | Never copy OpenCode credentials into native config    |
| Privacy                   | Provider may receive source text       | Local inference keeps content local    | Remote extraction must be explicit and visible        |

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

The existing `src/embedding.rs::Embedder` trait is a viable future adapter seam,
but a remote implementation would still need:

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

Using a hosted OpenCode provider changes the current privacy statement: source
memory content can leave the local machine. The following must be true before
enabling remote extraction:

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
8. A malformed or unsupported response is quarantined rather than retried
   without a limit.
9. Entity resolution never crosses project, agent, session, or repository trust
   boundaries without an explicit policy.
10. Graph export makes source provenance and derived status visible so users can
    delete or rebuild derived data.

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

Rejected for the first phase. Hindsight's storage is tightly coupled to
PostgreSQL extensions and its own operational model. The native daemon already
has a local zvec/state/journal design and does not need a second database.

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

- one version-pinned OpenCode structured-output call;
- extraction JSON Schema and fallback parser;
- English/Vietnamese/code fixtures with expected entities and relations;
- provider failure, malformed JSON, timeout, and cancellation tests;
- redaction and remote-eligibility check before provider dispatch;
- cost, latency, and output-size measurements;
- no graph persistence.

Exit criteria:

- the configured provider can return a bounded schema reliably;
- the exact SDK response field is verified for the supported OpenCode version;
- malformed output never reaches native graph persistence;
- explicit provider opt-in is observable to the user;
- local provider and hosted provider behavior are documented separately.

### Phase 1: Native graph sidecar and explicit tool

Deliverables:

- Rust graph value types and validation;
- graph sidecar manifest and pending journal;
- prepare/upsert/status/export domain contracts;
- TypeScript provider bridge and `memory_graph_extract` tool;
- deterministic entity resolution within scope;
- source hash and evidence validation;
- graph status and diagnostics.

Exit criteria:

- one project actor remains the only graph writer;
- a daemon restart recovers or discards pending graph writes safely;
- source memory deletion does not leave unsupported active facts;
- graph export can rebuild graph state without provider credentials;
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
- graph expansion does not bypass scope, lifecycle, or quarantine filters;
- graph retrieval improves the frozen benchmark without regressing existing
  lexical/dense/hybrid retrieval.

### Phase 3: Durable background extraction

Only after the explicit tool is reliable:

- persist extraction jobs and source revisions in the native daemon;
- let a plugin worker claim and renew a bounded job lease;
- make retries idempotent by extraction run ID and source hash;
- resume after plugin or daemon restart;
- keep remote extraction disabled for scopes that do not permit egress.

### Phase 4: Optional embedding backends

This is independent of the graph sidecar:

- evaluate a direct OpenAI-compatible embedding endpoint or native worker;
- introduce an explicit `EmbeddingSpaceId` and model-space migration;
- store incompatible vectors separately;
- benchmark local vs remote quality, cost, latency, and privacy;
- retain local fallback when the provider is unavailable.

## Acceptance Criteria

The knowledge graph feature should not be considered complete until:

1. The current memory APIs and local embedding behavior remain compatible.
2. Existing zvec collections open without graph extraction or model migration.
3. Graph writes are journaled and recoverable through the project actor.
4. The graph can be rebuilt from source memory IDs, provenance, and extraction
   runs.
5. Entity and relation IDs are assigned natively, never trusted from the LLM.
6. Graph relations remain separate from lifecycle supersession/conflict links.
7. Every active relation has source evidence and a scope boundary.
8. Remote provider use is explicit, bounded, and visible.
9. Provider credentials never enter native configuration or graph state.
10. Malformed, unsafe, stale, or out-of-scope candidates are rejected or
    quarantined without mutating active graph state.
11. Graph retrieval uses rank fusion and never compares incompatible raw vector
    scores.
12. Current memory retrieval remains useful when the provider is unavailable.
13. Temporal invalidation preserves historical provenance.
14. Tests cover duplicate ingestion, entity collision, contradiction, deletion,
    scope isolation, restart recovery, provider timeout, and no-answer queries.

## Open Questions

These should be decided before the Protobuf contract is frozen:

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
6. Should graph extraction use the current user session, or must the plugin
   create an isolated extraction session to avoid adding extraction messages to
   the user's conversation history?
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
- [Hindsight OpenAPI 0.8.5](https://hindsight.vectorize.io/openapi.json)
- [Hindsight repository license](https://github.com/vectorize-io/hindsight/blob/main/LICENSE)

### Graphiti

- [Graphiti repository](https://github.com/getzep/graphiti)
- [Graphiti README and context graph model](https://github.com/getzep/graphiti/blob/main/README.md)
- [`Graphiti` orchestration](https://github.com/getzep/graphiti/blob/main/graphiti_core/graphiti.py)
- [Graphiti graph types](https://github.com/getzep/graphiti/blob/main/graphiti_core/graphiti_types.py)
- [LLM client abstraction](https://github.com/getzep/graphiti/blob/main/graphiti_core/llm_client/client.py)
- [OpenAI-compatible LLM client](https://github.com/getzep/graphiti/blob/main/graphiti_core/llm_client/openai_generic_client.py)
- [Embedding client abstraction](https://github.com/getzep/graphiti/blob/main/graphiti_core/embedder/client.py)
- [Hybrid search implementation](https://github.com/getzep/graphiti/blob/main/graphiti_core/search/search.py)
- [Search configuration recipes](https://github.com/getzep/graphiti/blob/main/graphiti_core/search/search_config_recipes.py)
- [Graphiti Apache-2.0 license](https://github.com/getzep/graphiti/blob/main/LICENSE)

### OpenCode

- [OpenCode plugins](https://opencode.ai/docs/plugins/)
- [OpenCode SDK](https://opencode.ai/docs/sdk/)
- [OpenCode providers](https://opencode.ai/docs/providers/)
- [OpenCode permissions](https://opencode.ai/docs/permissions/)
- [OpenCode plugin API source](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/plugin/src/index.ts)
- [OpenCode prompt execution](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/opencode/src/session/prompt.ts)
- [OpenCode LLM design and embedding scope](https://github.com/anomalyco/opencode/blob/v1.18.9/packages/llm/DESIGN.md)

### Local repository references

- [`MemoryRecord`](../src/contract.rs)
- [Native embedding abstraction](../src/embedding.rs)
- [Memory engine](../src/engine/mod.rs)
- [Lifecycle state](../src/storage/state.rs)
- [zvec storage](../src/storage/zvec.rs)
- [Document ingestion](../src/document.rs)
- [TypeScript plugin](../opencode-memory/src/plugin.ts)
- [Daemon client](../opencode-memory/src/daemon-client.ts)
- [Multi-model embedding plan](./multi-model-embedding-plan.md)
- [Project embedding model switch plan](./project-embedding-model-switch-plan.md)
