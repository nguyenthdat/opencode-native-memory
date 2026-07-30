# Audit of Current Memory Gaps Versus Hindsight and Graphiti

Audit date: 2026-07-30

Code scope: current working tree on branch `main`, baseline commit
`ce9b6c2`. Existing local changes in README/TUI are out of scope for this
audit.

Comparison scope:

- Hindsight OSS 0.8.x: `retain`, `recall`, `observations`, `reflect`, and source
  memory management.
- Graphiti OSS 0.29.3: episodic ingestion, temporal knowledge graph, entity/edge
  resolution, and hybrid graph search.
- Current project: OpenCode TypeScript plugin, Rust daemon, zvec, local
  llama.cpp embeddings, and native graph sidecar.

This document focuses on the shortcomings of the current project. It does not
propose replacing the native daemon with Hindsight/PostgreSQL or
Graphiti/Python/Neo4j.

## Executive Conclusion

The current project already has a local-first memory foundation that is more
capable than a simple vector store:

- memory and lifecycle are persisted by an actor/writer;
- local embedding, dense/lexical/hybrid retrieval, MMR, and context packing;
- session/agent/project/repository scope;
- source provenance, content hash, extraction revision, and code-anchor staleness;
- LLM graph extraction through the OpenCode provider in an isolated session;
- durable graph jobs, leases, retries, cancellation, and crash recovery;
- temporal graph relations, bounded BFS, and graph-to-memory RRF;
- source deletion/purge deletes graph evidence before deleting the source.

However, the project is **not yet an LLM-powered memory learning system in the
Hindsight sense**. The LLM is currently used only in explicit graph extraction.
The normal memory write path is still:

```text
manual memory_store
or compaction summary with a curated candidate block
or document chunking
  -> a MemoryRecord whose content and taxonomy were pre-defined by the caller/assistant
  -> local embedding
  -> zvec/lifecycle state
```

It does not yet have the following path:

```text
conversation/document/tool outcome
  -> LLM fact extraction
  -> world/experience fact + event time + entity + causal link
  -> entity/edge resolution
  -> evidence-backed observation consolidation
  -> semantic/lexical/temporal/graph retrieval
  -> reflect with citations
```

The three most important gaps are:

1. **No automatic write-time LLM retain pipeline.** Document indexing only
   stores chunks; compaction accepts at most three candidates created by the
   assistant; graph extraction must be called explicitly.
2. **No first-class extracted fact and observation layer.** The entity/relation
   graph exists, but there are no world/experience facts, fact event time, proof
   count, observation freshness, or consolidation worker.
3. **Graph retrieval is still lexical/BFS.** There is no graph/entity/fact
   embedding, temporal query parsing, BM25/cross-encoder reranking, or retrieval
   dedicated to observations/episodes like Hindsight and Graphiti.

If the goal is "LLM self-learning memory", the right foundation to prioritize
is adding a source-backed fact pipeline and observation pipeline on top of the
current architecture. Do not start by changing the embedding model or storage
backend.

## Correct Understanding of the Two Reference Systems

### Hindsight

Hindsight is a more complete memory product than a graph engine:

- `retain()` uses an LLM at write time to extract facts, entities, event time,
  context, and causal links;
- facts are classified as world facts and agent experience;
- `recall()` runs semantic, keyword, graph, and temporal retrieval, fuses with
  RRF, reranks, and packs according to a token budget;
- observation consolidation runs in the background and creates derived
  knowledge with source facts/proof count;
- `reflect()` uses an LLM to synthesize answers according to a hierarchy of
  mental models, observations, and raw facts;
- source documents, chunks, facts, and observations have provenance and
  lifecycle.

Hindsight recall is designed not to require an LLM. LLM cost is concentrated in
retain, consolidation, and reflect.

### Graphiti

Graphiti is a temporal context-graph engine, not a complete memory product:

- sources are ingested as `message`, `text`, or `json` episodes;
- the LLM extracts entities, edges/facts, timestamps, attributes, and entity
  summaries;
- entity resolution uses embedding candidates, deterministic matching, and LLM
  adjudication for ambiguous cases;
- edge resolution includes duplicate/contradiction detection and temporal
  invalidation;
- the graph has event/valid time and transaction/system time;
- search has full text, cosine, graph traversal, RRF/MMR/node-distance, and
  optional cross-encoder recipes;
- the LLM, embedder, reranker, and graph driver are separate seams.

Graphiti OSS has entity/community/saga summary mechanisms, but **it should not
be equated with Hindsight observations or Zep managed Observations**. Zep is a
managed product layer built on Graphiti with additional
user/thread/context/observation/enterprise capabilities.

## Current Project Baseline

### Write path

| Write path            | Current implementation                                                                                             | Assessment                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------- |
| Manual memory         | `memory_store` receives already-distilled content, kind, taxonomy, importance, scope, and personalization evidence | Good for curated memory, not automatic learning                                                                     |
| Automatic capture     | When `session.compacted`, parses up to three curated candidates from the assistant summary                         | Indirect LLM use through the compaction model, but no separate extractor and no processing of the full event stream |
| Document ingest/index | xberg extraction, Markdown chunking, each chunk stored as a `Fact` with default importance/confidence `0.6`        | Searchable source archive, not yet fact extraction                                                                  |
| Graph extraction      | An explicit tool or durable job calls the active OpenCode provider and requests JSON Schema entities/relations     | Real LLM extraction exists, but the schema and trigger remain narrow                                                |
| Outcome capture       | Has `task_attempt`/`tool_call` taxonomies, but no producer from task/tool/session outcomes                         | The taxonomy is only a label, not episodic learning                                                                 |

Code evidence:

- The compaction candidate policy limits candidates to three and importance to
  `<= 0.6` at [`opencode-memory/src/policy.ts:7-12`](../opencode-memory/src/policy.ts) and
  [`opencode-memory/src/policy.ts:135-213`](../opencode-memory/src/policy.ts).
- The capture hook runs only after `session.compacted` at
  [`opencode-memory/src/plugin.ts:678-729`](../opencode-memory/src/plugin.ts).
- The document index creates `MemoryKind::Fact` with default `0.6` and passes
  content chunks directly into ingest at [`src/engine/mod.rs:567-732`](../src/engine/mod.rs).
- The capture gate is described as pure deterministic logic with no LLM call at
  [`src/capture.rs:1-12`](../src/capture.rs).
- The taxonomy includes `TaskAttempt`, `ToolCall`, and `SessionSummary`, but
  these only map to a family/retrieval profile at [`src/taxonomy.rs:13-44`](../src/taxonomy.rs) and
  [`src/taxonomy.rs:105-167`](../src/taxonomy.rs).

### LLM provider path

The project has already solved many difficult safety concerns:

- the active provider/model is obtained from a normal chat message;
- extraction runs in a separate session;
- all permissions are denied and tools are disabled;
- input is wrapped as JSON-encoded untrusted source units;
- output must conform to JSON Schema;
- source, output size, retries, and timeout are all bounded;
- extraction sessions are always cleaned up;
- the native daemon revalidates source hash, revision, scope, and evidence.

Evidence:

- Prompt/schema and candidate bounds at
  [`opencode-memory/src/graph-extractor.ts:8-91`](../opencode-memory/src/graph-extractor.ts).
- Isolated session/provider call at
  [`opencode-memory/src/graph-extractor.ts:242-335`](../opencode-memory/src/graph-extractor.ts).
- Explicit permission, remote eligibility, dry-run, and durable enqueue at
  [`opencode-memory/src/plugin.ts:971-1122`](../opencode-memory/src/plugin.ts).
- Native source/revision/provenance validation at
  [`src/engine/knowledge_graph.rs:953-1121`](../src/engine/knowledge_graph.rs).

This is a strong foundation. The gap is not that the "project does not use
LLMs"; it is that the LLM only creates graph candidates when the user calls a
tool, rather than participating in a learning pipeline with fact/observation
lifecycle.

### Graph and retrieval path

The current native graph state has entities, relations, mentions, runs, and
jobs. It does not have fact/observation/community collections:

- [`src/graph/mod.rs:224-303`](../src/graph/mod.rs)
- [`schema/opencode/memory/graph/v1/graph.proto:38-183`](../schema/opencode/memory/graph/v1/graph.proto)

Graph search:

- lexical seed over entity name/type/aliases and relation predicate/type/evidence;
- bounded BFS to depth 2/fanout 32;
- eligibility is filtered before traversal;
- results are projected to source memory IDs;
- source IDs are fused with normal memory rank using RRF;
- MMR and character-budget packing run afterward.

Evidence:

- Lexical/BFS graph search at
  [`src/graph/mod.rs:1070-1223`](../src/graph/mod.rs).
- Source-visible graph result/provenance at
  [`src/engine/knowledge_graph.rs:566-735`](../src/engine/knowledge_graph.rs).
- Dense/lexical/hybrid search, graph fusion, MMR, and context packing at
  [`src/engine/retrieval.rs:27-169`](../src/engine/retrieval.rs) and
  [`src/engine/retrieval.rs:268-364`](../src/engine/retrieval.rs).

### Lifecycle and provenance

This is the strongest part of the current project:

- source memory is the source of truth;
- graph evidence is bound to source ID, content hash, extraction revision, scope,
  and policy revision;
- graph reads validate that the source remains visible/current;
- source update/delete/purge erases graph evidence;
- durable jobs are fenced when the source changes;
- user deletion does not leave a hidden queryable graph copy.

Compared with Hindsight/Graphiti, this is not a major gap. The current
architecture is better suited to local-first and OpenCode than copying the
storage model of either project.

## Gap Matrix

Status:

- `Present`: the capability is usable in the code.
- `Partial`: some primitive exists, but it does not yet provide the reference
  system's behavior.
- `Missing`: there is no corresponding implementation path.

| Capability                  | Current project                      | Hindsight                          | Graphiti OSS                                           | Main gap                                                                     | Priority |
| --------------------------- | ------------------------------------ | ---------------------------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------- | -------- |
| Local durable memory        | Present                              | Present                            | Requires a separate graph backend                      | Not significant                                                              | Maintain |
| LLM write-time extraction   | Partial, explicit graph only         | Present in `retain`                | Present in episode ingestion                           | Not running on ordinary conversation/document writes                         | P0       |
| First-class extracted facts | Missing                              | World/experience facts             | Entity edges/facts from episodes                       | Memory chunks and graph relations are not a complete fact layer              | P0       |
| Evidence/source provenance  | Present, strong                      | Present                            | Present                                                | Not significant                                                              | Maintain |
| Observation consolidation   | Missing                              | Present, async and proof-backed    | No Zep-style observations; narrower summary mechanisms | No durable derived knowledge                                                 | P0       |
| Entity resolution           | Partial, deterministic               | Semantic/contextual resolution     | Embedding + deterministic + LLM                        | Ambiguous aliases/entities are rejected instead of adjudicated               | P1       |
| Bi-temporal semantics       | Partial on relations                 | Occurred vs mentioned/learned time | Valid vs transaction time                              | Ordinary memory/source episodes have no event/reference time                 | P1       |
| Graph semantic retrieval    | Missing                              | Semantic/keyword/graph/temporal    | Full text/vector/BFS recipes                           | Graph is only lexical/BFS, with no entity/fact vectors                       | P1       |
| Reranking                   | Partial through scoring/MMR          | Cross-encoder                      | RRF/MMR/cross-encoder options                          | No learned reranker or graph-reranking benchmark                             | P2       |
| Reflect/reasoning API       | Missing                              | Present                            | Not a general core capability                          | OpenCode model reasons over recalled context, but no cited reflect operation | P1       |
| Tool/task outcome learning  | Missing                              | Experience facts                   | Can be ingested as episodes                            | Taxonomy exists but has no automatic event producer                          | P1       |
| Retrieval feedback          | Present, injected/used/ignored/error | Has separate ranking/usage signals | Not a product-level feedback loop                      | Feedback only adjusts rank; it does not create experience/observation        | P1       |
| Durable extraction jobs     | Present for graph                    | Has operations queue               | Operator-dependent                                     | Document ingest queue remains in memory                                      | P1       |
| Custom ontology/attributes  | Missing                              | Entity labels/missions             | Custom Pydantic entity/edge types                      | Current schema has seven predicates and string entity type                   | P2       |
| Provider seams              | Partial                              | LLM/embed/rerank providers         | LLM/embed/rerank/driver interfaces                     | OpenCode provider adapter is only for graph extraction                       | P2       |
| Quality/cost observability  | Partial                              | Operation/tracing surface          | Tracing/provider metrics available by deployment       | No provider corpus evaluation or run-level token/latency/quality metrics     | P0       |
| Large graph scalability     | Limited by 16 MiB JSON sidecar       | PostgreSQL/Oracle                  | Neo4j/FalkorDB/Neptune                                 | No measured migration threshold                                              | P2       |

## Detailed Gap Analysis

### G1. No automatic LLM retain pipeline

Priority: **P0**

Current:

- `memory_store` requires the caller to provide distilled content;
- automatic capture depends on a compaction summary containing a custom candidate
  block;
- document ingestion only chunks and embeds;
- `memory_graph_extract` requires explicit source IDs, an active model, and
  permission;
- automatic document indexing does not enqueue graph extraction.

Missing:

- a common ingestion contract for messages/documents/tool outcomes;
- extraction of fact text, who/what/when/where/why, fact type, and causal links;
- configurable retain mission/policy;
- trigger policy by source type and privacy scope;
- batch/cost quotas by session/project/provider;
- automatic durable scheduling when a source is admitted.

Consequences:

- memory quality depends on whether the assistant writes a curated candidate on
  its own;
- many decisions, tool results, and document knowledge exist only as coarse
  chunks;
- the graph is often sparse unless the user knows the source ID and calls the
  tool;
- the project has a durable graph worker but no learning loop that uses it.

Minimum fix direction:

1. Add a provider-neutral `retain` pipeline, reusing graph prepare/source
   eligibility and durable lease primitives.
2. Keep hosted-provider use explicit/opt-in by default. Enable automatic mode
   only when project policy permits that source class and egress mode.
3. Separate extraction candidates from persistence. The native daemon should
   continue to assign IDs, validate scope/evidence, and commit.
4. Add `dry_run` and corpus benchmarks before enabling automatic mode.

Exit criteria:

- a conversation message, document chunk, or tool outcome can create bounded fact
  candidates in a durable job;
- provider failure does not lose the source or create partial facts;
- source mutation fences the job and invalidates derived facts;
- the user can see source count, bytes, provider/model, status, and cost/usage if
  returned by the provider.

### G2. No first-class fact layer

Priority: **P0**

There are currently two layers:

- `MemoryRecord`: a raw/curated text record with lifecycle and vector;
- graph entity/relation: source-backed derived graph facts.

The graph relation schema has only a subject, one of seven predicates, an object,
`relation_type`, optional `valid_at`/`invalid_at`, evidence, and confidence. It
does not fully represent a narrative fact such as:

- who did what;
- why and in what context;
- whether this is a world fact or agent experience;
- during what time interval the fact occurred;
- which episode/message/document/tool result the fact came from;
- which fact caused or supported which other fact.

Hindsight stores facts as retrieval units. Graphiti stores episodes as ground
truth and entity edges/facts as derived graph units. The current project uses
`MemoryRecord` as both the source episode and search record, and a relation
cannot replace a fact layer.

Minimum fix direction:

```text
DerivedFact
  fact_id
  source_memory_id / source_unit_id / source_revision
  text
  fact_type: world | experience
  context / rationale (bounded, optional)
  occurred_start_ms / occurred_end_ms
  mentioned_at_ms / extracted_at_ms
  entity_ids
  causal_fact_ids
  confidence
  provider/prompt/schema identity
  status: active | invalidated | stale
  exact evidence
```

Fact text remains derived data. The source `MemoryRecord` remains authoritative,
and deleting a source must delete or invalidate the fact according to the
current policy.

### G3. No observation consolidation

Priority: **P0**

Graph state only has entities, relations, mentions, runs, and jobs. The deferred
list also explicitly states that observation/community summaries have not been
implemented at [`docs/knowledge-graph-implementation.md:44-53`](knowledge-graph-implementation.md).

Missing:

- a worker that combines multiple facts into a durable observation;
- source fact IDs and proof count;
- a freshness watermark indicating which extraction revision the observation has
  been processed through;
- merge/refine/retire behavior when new or contradictory evidence arrives;
- scope rules to avoid consolidating session/agent/private facts across the wrong
  boundary;
- review/edit/invalidate/restore flows for derived observations;
- retrieval/ranking dedicated to observations.

Consequences:

- the system remembers many records but does not form stable knowledge on its own;
- repeated evidence does not increase confidence through an explainable
  mechanism;
- contradictions exist as relations/memory links but do not automatically rebuild
  conclusions;
- on every recall, the model must synthesize again from raw chunks.

Minimum fix direction:

```text
GraphObservation
  observation_id
  statement
  scope
  source_fact_ids
  source_memory_ids
  proof_count
  confidence
  consolidated_through_revision
  freshness: current | stale | rebuilding
  supersedes_observation_ids
  provider/prompt/schema identity
```

An observation must not replace source facts. Recall should return the observation
alongside source facts so the OpenCode model can verify it.

### G4. Deterministic-only entity resolution

Priority: **P1**

Current resolution:

- NFKC/lowercase/whitespace normalization;
- exact normalized name or alias;
- token Jaccard `>= 0.9` within the same scope and entity type;
- if there is more than one match, reject ambiguous resolution.

Evidence:

- [`src/graph/mod.rs:1806-1870`](../src/graph/mod.rs)
- [`src/graph/mod.rs:1975-2024`](../src/graph/mod.rs)

Compared with Graphiti/Hindsight, the following are missing:

- entity-name/summary embeddings to retrieve candidates;
- co-occurrence and temporal context;
- deterministic exact/fuzzy score trace;
- LLM adjudication only for the remaining ambiguity;
- merge/split/review workflow and history;
- entity summary/profile.

Do not call an LLM for every entity. The order appropriate for the current
architecture is:

1. exact normalized ID;
2. exact alias;
3. bounded token/fuzzy match;
4. local entity embedding candidate search;
5. optional LLM adjudication with explicit confidence and review receipt.

### G5. Temporal model limited to graph relations

Priority: **P1**

The current `MemoryRecord` only has `created_at_ms` and `updated_at_ms` at
[`src/contract.rs:487-525`](../src/contract.rs). Graph relations have
`valid_at_ms`, `invalid_at_ms`, `created_at_ms`, and `extracted_at_ms`, and graph
search has an as-of filter.

Missing:

- event/occurred time on ordinary facts/source episodes;
- a separate mentioned/learned time;
- caller-supplied `reference_time` and timezone to resolve "yesterday"/"last
  week";
- occurred intervals on facts;
- temporal query parser/window retrieval;
- transaction invalidation time separate from real-world invalid time.

`parseGraphTime()` currently only uses JavaScript `Date.parse` on provider output
at [`opencode-memory/src/plugin.ts:2191-2197`](../opencode-memory/src/plugin.ts). It
has no reference timestamp and does not resolve relative dates deterministically.

These must be separated:

```text
occurred_start / occurred_end   # when the fact held in the world
mentioned_at                    # when the source stated it
learned_at / extracted_at       # when the system learned it
invalid_at                      # when the fact stopped being valid
expired_at                      # when the system marked the record historical
```

### G6. Graph retrieval is only lexical/BFS

Priority: **P1**

Normal memory retrieval already has dense + lexical + rank calibration +
lifecycle + feedback + MMR. The graph channel only has lexical matching and BFS,
with a score trace named `lexical_bfs`.

Missing compared with Hindsight/Graphiti:

- entity/fact/observation vector index;
- full-text/BM25-style graph channel;
- independent temporal retrieval channel;
- causal/typed traversal weighting;
- node-distance, episode-mention, and relation-type ranking;
- optional cross-encoder reranking;
- token-aware packing; the project currently uses a character budget;
- retrieval evaluation for graphs, temporal facts, and observations.

Do not mix raw cosine scores from different models. If graph/entity embeddings
are added, use an immutable graph embedding generation or reuse a verified active
generation, then fuse ranked lists with RRF.

The current frozen benchmark has only six queries, including one negative query,
at [`tests/benchmark/retrieval-v1/queries.jsonl`](../tests/benchmark/retrieval-v1/queries.jsonl).
It does not measure:

- entity alias resolution;
- multi-hop graph retrieval;
- temporal current/as-of correctness;
- contradiction/invalidation;
- observation retrieval;
- Vietnamese temporal/entity cases;
- no-answer behavior after graph expansion.

### G7. No learning from tool/task/session outcomes

Priority: **P1**

The project has `task_attempt` and `tool_call` taxonomies, but no hook/pipeline
that turns tool execution results, task success/failure, test outcomes, or user
corrections into experience facts.

Current feedback only records:

- `injected`;
- `used`;
- `ignored`;
- `error`.

It is used as a ranking modifier, not to create episodic memory or a procedural
observation. Evidence is at
[`opencode-memory/src/session-context.ts:89-132`](../opencode-memory/src/session-context.ts)
and [`src/engine/mod.rs:1454-1550`](../src/engine/mod.rs).

Add a bounded event-to-experience pipeline for:

- task intent;
- tool/command and sanitized arguments;
- outcome/error class;
- affected files/symbols;
- user correction/approval;
- lesson candidate and evidence;
- session/task scope.

Do not store raw tool logs, secrets, or transient output. The LLM should only
distill a redacted and bounded event bundle.

### G8. No reflect operation with citations

Priority: **P1**

The OpenCode model can reason over recalled `<project-memory>`, but the project
has no operation equivalent to Hindsight `reflect()` that can:

- query observations/facts/raw sources in a hierarchy;
- iterate retrieval when evidence is insufficient;
- synthesize an answer with source IDs;
- return a trace showing which memories were used;
- support structured output;
- apply separate missions/directives for the memory bank/project.

A `memory_reflect` tool should be optional and explicit. It should run through the
OpenCode provider adapter, not in the Rust daemon. The native daemon should only
prepare bounded evidence and validate references; the provider creates the
synthesis.

There is no need to use an LLM in ordinary recall. Hindsight likewise separates
recall from reflect to keep latency/cost stable.

### G9. Background durability is inconsistent

Priority: **P1**

Graph extraction jobs are already durable and have lease/retry/recovery.
Document ingestion jobs are still held in a TypeScript `Map`, are cleared when
the plugin is disposed, and are lost when the process restarts:

- [`opencode-memory/src/background-jobs.ts:52-128`](../opencode-memory/src/background-jobs.ts)
- [`opencode-memory/src/background-jobs.ts:182-237`](../opencode-memory/src/background-jobs.ts)

If automatic retain/consolidation is added on this queue, a process restart can
lose learning work. Important storage-owning or provider-owning jobs should use
durable native job state and the lease pattern already used by graph extraction.

### G10. Extraction schema is too narrow

Priority: **P2**

The current extractor returns only:

- entity mention/canonical hint/type/aliases/evidence/confidence;
- relation subject/predicate/object/type/time/evidence/confidence;
- seven predicates: `uses`, `depends_on`, `implements`, `causes`, `related_to`,
  `supports`, `contradicts`.

Missing:

- custom ontology/entity/edge attributes;
- fact type and source episode type;
- project-specific extraction mission;
- entity labels and controlled vocabularies;
- entity summary;
- context/rationale/location/participant roles;
- causal fact-to-fact links;
- schema migration and re-extraction policy when the ontology changes.

Do not open the schema to arbitrary JSON immediately. Add versioned typed
extensions and an allowlist according to project policy.

### G11. No quality, cost, or drift evaluation for LLM memory

Priority: **P0**

Current tests demonstrate adapter safety and malformed-output handling with a fake
client, for example
[`opencode-memory/tests/graph-extractor.test.ts:101-183`](../opencode-memory/tests/graph-extractor.test.ts).
There is no end-to-end benchmark from real-provider extraction through native
persistence to graph recall.

The following metrics are needed before auto-enable:

- extraction precision/recall on English, Vietnamese, and code-heavy corpora;
- unsupported/hallucinated fact rate;
- exact-evidence pass/reject rate;
- entity duplicate/incorrect-merge/ambiguous rate;
- temporal parse accuracy;
- contradiction detection accuracy;
- observation faithfulness and stale rate;
- recall nDCG/MRR/Hit@K/no-answer precision;
- provider latency, retries, timeout, input/output tokens, and estimated cost;
- prompt/schema/model version drift.

Each extraction/consolidation run can store bounded metrics, but must not store
credentials or raw provider output.

### G12. Graph sidecar has no benchmarked scale path

Priority: **P2**

The whole-state graph JSON is bounded at 16 MiB and committed through atomic
replacement. This is suitable for the first native phase and preserves a single
writer, but it is not equivalent to the storage scale of PostgreSQL/Hindsight or
Graphiti's graph backend.

Missing:

- graph size/commit latency benchmarks;
- compaction and fragmentation policy;
- threshold for switching to an embedded transactional store;
- online migration/rollback;
- index rebuild benchmarks;
- large-project fanout/latency SLO.

This is not a reason to introduce Neo4j/PostgreSQL immediately. Measure the
actual sidecar limits first, then choose a native embedded store if needed.

## What Not to Copy

### Do not replace native storage with Hindsight

Hindsight's PostgreSQL/Oracle architecture fits service deployment, but not the
project's local-first, one-user daemon, actor-owned writer, and Markdown
repository memory goals. Learn the `retain/recall/reflect` behavior, not the
backend.

### Do not embed the Graphiti Python runtime

Graphiti requires a graph driver/backend and a Python runtime. Bringing it into
the daemon creates additional writer, deployment, and crash-recovery boundaries.
Learn the episode, bi-temporal fact, resolution, and search recipes, but
implement them according to current native ownership.

### Do not use a chat model as an embedding model

The OpenCode provider adapter is an LLM extraction seam, not a generic embedding
API. Local embedding generation and model-switch invariants must remain
independent.

### Do not automatically send every document to a hosted provider

Automatic retain must have an egress policy, source class, quota, and user-visible
status. Repository-scoped and code-backed sources are currently remote-ineligible;
do not remove this guard to achieve feature parity.

### Do not create observations without evidence

An observation is only a derived view. It must have source fact IDs, source memory
IDs, proof/freshness, and rebuild/invalidation semantics. A generated summary
without citations must not be treated as durable truth.

## Proposed Target Architecture

```text
OpenCode process
  source/event collector
    -> bounded, redacted source units
  provider bridge
    -> FactCandidate[]
    -> optional EntityResolutionDecision[]
    -> optional ObservationCandidate[]
    -> optional ReflectResult

Rust project actor
  authoritative source memory
    -> zvec + lifecycle + document ownership
  derived fact store
    -> source revision + event/learned time + evidence
  graph store
    -> entities + relations + mentions + temporal history
  observation store
    -> evidence-backed consolidated knowledge
  durable jobs
    -> retain + resolve + consolidate + rebuild
  retrieval
    -> memory dense/lexical
    -> fact/entity/observation ranked channels
    -> temporal/graph expansion
    -> RRF + lifecycle/scope filters + MMR + budget packing
```

Ownership should remain as it is now:

- TypeScript only calls the provider and returns untrusted candidates;
- Rust derives scope, assigns IDs, validates evidence, and persists;
- source memory remains authoritative;
- derived facts/graph/observations must not outlive the source policy;
- provider credentials must not enter native state.

## Priority Roadmap

### Phase A: Fact retain foundation

Goal: create a safe fact layer without enabling automatic mode.

1. Add versioned `FactCandidate` and `DerivedFact` domains.
2. Add occurred/mentioned/learned time and source reference time.
3. Extend the provider prompt/schema to extract world/experience facts, entities,
   causal links, and evidence.
4. Reuse graph source preparation, remote eligibility, durable jobs, and native
   candidate validation.
5. Add dry-run and fixture benchmarks for English/Vietnamese/code.
6. Add graph/fact invalidation to all source mutation paths.

Definition of done:

- explicit retain creates source-backed facts idempotently;
- fact revalidation and user deletion do not leave a hidden copy;
- provider failures/malformed output/timeouts do not mutate state;
- benchmarks and run metrics are persisted in bounded form.

### Phase B: Controlled automatic retain

Goal: self-learning memory with clear policy.

1. Add project settings for source classes, local/hosted provider, and quota.
2. Enqueue after selected events: document ingest complete, session compaction,
   verified tool/task outcome.
3. Do not enqueue repository/code-backed content without separate consent.
4. Add backpressure, rate limits, and user-visible pending/failure status.
5. Add a re-extraction command by provider/prompt/schema revision.

Definition of done:

- automatic mode is opt-in, observable, and reversible;
- no hidden egress;
- restart does not lose jobs;
- duplicate source/revision does not create duplicate facts.

### Phase C: Observation consolidation

Goal: turn repeated facts into durable knowledge with evidence.

1. Add `GraphObservation`/`ObservationStore` and source-fact references.
2. Add consolidation watermark, proof count, freshness, and stale/rebuild state.
3. Add merge/refine/retire behavior when contradictory/new facts exist.
4. Add observation search and citation expansion to raw facts/source memories.
5. Add review/edit/invalidate/restore flows.

Definition of done:

- an observation without active evidence cannot be recalled;
- source update/delete makes observations stale/rebuild correctly;
- the faithfulness benchmark meets its threshold before automatic consolidation.

### Phase D: Retrieval and reflection

Goal: exploit facts/graph/observations without making ordinary recall too expensive.

1. Add a fact/entity/observation semantic index by immutable generation.
2. Add a temporal retrieval channel and query/reference time.
3. Fuse lexical/dense/temporal/graph/observation ranks with explainable RRF.
4. Evaluate an optional local/remote cross-encoder reranker.
5. Add explicit `memory_reflect` with bounded evidence, citations, and structured
   output.
6. Extend the frozen benchmark for graph, temporal, contradiction, observation,
   and Vietnamese cases.

## Criteria for Calling It LLM-Powered Memory

The project should describe itself as having full LLM-powered memory learning
only when all of the following are true:

1. An ordinary source can be explicitly or policy-driven retained as facts,
   without the user having to write a `memory_store` payload.
2. Facts have source evidence, source revision, event time, learned time, type, and
   provider/schema identity.
3. Entity resolution has an explainable candidate path and safe ambiguity handling.
4. Derived observations have proof/freshness and rebuild/invalidation semantics.
5. Recall has at least lexical, dense, temporal, and graph/observation channels
   with no-answer protection.
6. Reflect has citations and does not turn generated synthesis into source truth.
7. Tool/task outcomes can create bounded experience facts without storing raw logs
   or secrets.
8. Provider egress, cost, retry, and quality are observable.
9. Restart does not lose retain/consolidation jobs.
10. User update/delete/purge deletes or invalidates every derived copy according to
    policy.

By these criteria, the current project meets item 8 partially, item 9 for graph
jobs, and item 10; it partially meets items 2, 3, and 5; it does not yet meet
items 1, 4, 6, and 7.

## Risks of Getting the Order Wrong

| Risk                       | If done incorrectly                                      | Guardrail                                                           |
| -------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------- |
| Hidden source egress       | Auto-extract every document with the active hosted model | Explicit policy by source class/provider mode; permission and quota |
| Hallucinated durable facts | Persist raw LLM output                                   | Exact evidence + native validation + confidence + dry-run           |
| Entity over-merge          | LLM or fuzzy matching merges across scopes               | Scope/type hard boundary, candidate trace, ambiguity review         |
| Stale observations         | Consolidation is not bound to source revision            | Watermark + source-fact IDs + stale/rebuild state                   |
| Cost explosion             | LLM extraction/consolidation for every chunk             | Delta retain, batching, dedupe, rate limit, operation budget        |
| Recall latency             | LLM in every recall                                      | Keep recall LLM-free; reranker/reflect optional                     |
| Data duplication           | Fact/graph/observation becomes a second source of truth  | Source memory authoritative; derived erasure/invalidation           |
| Lost work after restart    | Use a TypeScript in-memory queue                         | Native durable jobs + expiring leases                               |
| Vector corruption          | Mix entity/fact vectors from different models            | Immutable generation + rank fusion; do not mix raw scores           |
| Unbounded sidecar          | Observation/fact growth in JSON                          | Metrics, cap, compaction, and measured storage migration            |

## Source Audit

### Local code

- [Knowledge graph implementation status](knowledge-graph-implementation.md)
- [Original Hindsight/Graphiti design plan](knowledge-graph-hindsight-graphiti-plan.md)
- [OpenCode graph extractor](../opencode-memory/src/graph-extractor.ts)
- [OpenCode plugin hooks and tools](../opencode-memory/src/plugin.ts)
- [Automatic capture policy](../opencode-memory/src/policy.ts)
- [Plugin-local background jobs](../opencode-memory/src/background-jobs.ts)
- [Native capture gate](../src/capture.rs)
- [Memory contract](../src/contract.rs)
- [Native memory engine](../src/engine/mod.rs)
- [Native retrieval](../src/engine/retrieval.rs)
- [Native graph integration](../src/engine/knowledge_graph.rs)
- [Native graph store](../src/graph/mod.rs)
- [Graph Protobuf contract](../schema/opencode/memory/graph/v1/graph.proto)
- [Memory taxonomy](../src/taxonomy.rs)

### Hindsight

Sources checked on 2026-07-30. Current Hindsight docs describe the 0.8 line;
the latest release cross-checked is v0.8.6.

- [Retain architecture](https://hindsight.vectorize.io/developer/retain)
- [Retain API](https://hindsight.vectorize.io/developer/api/retain)
- [Recall architecture](https://hindsight.vectorize.io/developer/retrieval)
- [Recall API](https://hindsight.vectorize.io/developer/api/recall)
- [Observations](https://hindsight.vectorize.io/developer/observations)
- [Reflect](https://hindsight.vectorize.io/developer/reflect)
- [Memory curation and invalidation](https://hindsight.vectorize.io/developer/api/memories)
- [Documents and provenance](https://hindsight.vectorize.io/developer/api/documents)
- [Models and providers](https://hindsight.vectorize.io/developer/models)
- [Hindsight v0.8.6](https://github.com/vectorize-io/hindsight/releases/tag/v0.8.6)
- [Pinned retain orchestrator](https://github.com/vectorize-io/hindsight/blob/cc1eaeeeba58ba7dad802cd456391df6a4708791/hindsight-api-slim/hindsight_api/engine/retain/orchestrator.py)
- [Pinned fact extraction](https://github.com/vectorize-io/hindsight/blob/cc1eaeeeba58ba7dad802cd456391df6a4708791/hindsight-api-slim/hindsight_api/engine/retain/fact_extraction.py)

Note: the 2025 Hindsight paper describes a separate opinion network. Current
OSS has removed the `opinion` fact type and uses observations/mental models.
This audit prioritizes current OSS/docs and does not treat the opinion network
in the paper as a current API parity target.

### Graphiti and Zep

Sources checked on 2026-07-30, Graphiti OSS v0.29.3.

- [Graphiti README, pinned source](https://github.com/getzep/graphiti/blob/71a719be482294dd4bbfc5cef557a3a6a500c134/README.md)
- [Adding episodes](https://help.getzep.com/graphiti/working-with-data/adding-episodes.md)
- [Searching the graph](https://help.getzep.com/graphiti/working-with-data/searching.md)
- [Custom entity and edge types](https://help.getzep.com/graphiti/core-concepts/custom-entity-and-edge-types.md)
- [LLM configuration](https://help.getzep.com/graphiti/configuration/llm-configuration.md)
- [Pinned Graphiti orchestration](https://github.com/getzep/graphiti/blob/71a719be482294dd4bbfc5cef557a3a6a500c134/graphiti_core/graphiti.py)
- [Pinned node extraction/resolution](https://github.com/getzep/graphiti/blob/71a719be482294dd4bbfc5cef557a3a6a500c134/graphiti_core/utils/maintenance/node_operations.py)
- [Pinned edge extraction/invalidation](https://github.com/getzep/graphiti/blob/71a719be482294dd4bbfc5cef557a3a6a500c134/graphiti_core/utils/maintenance/edge_operations.py)
- [Pinned search recipes](https://github.com/getzep/graphiti/blob/71a719be482294dd4bbfc5cef557a3a6a500c134/graphiti_core/search/search_config_recipes.py)
- [Zep vs Graphiti boundary](https://help.getzep.com/zep-vs-graphiti.md)
- [Zep Observations](https://help.getzep.com/observations.md)
- [Graphiti v0.29.3](https://github.com/getzep/graphiti/releases/tag/v0.29.3)

Note: the 2025 Graphiti paper and some other older docs differ from the current
source regarding reflection, bulk invalidation, and search defaults. This audit
prioritizes current v0.29.3 source.

## Proposed Decisions

1. Keep the native daemon, zvec, local embedding, and graph sidecar as the
   foundation.
2. Prioritize `DerivedFact` + an explicit retain benchmark before observations,
   reranking, or remote embedding.
3. Reuse the OpenCode provider bridge and durable graph job model; do not create a
   provider client in Rust.
4. After fact quality meets its threshold, add controlled automatic retain.
5. Only then add evidence-backed observation consolidation.
6. Finally, expand graph semantic retrieval and optional `memory_reflect`.

This is the shortest path for the project to move from "durable local memory with
optional LLM graph extraction" to an "LLM-powered memory learning system" while
preserving privacy, one-writer persistence, and the current source-of-truth
invariants.
