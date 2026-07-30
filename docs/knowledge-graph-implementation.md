# Knowledge Graph Implementation

Implemented: 2026-07-29

The knowledge graph implementation now covers the native sidecar, temporal and
rank-fused retrieval, and durable leased extraction jobs from Phases 1-3 of
`knowledge-graph-hindsight-graphiti-plan.md`. It is additive: zvec, lifecycle
state, local embeddings, Markdown-backed repository memory, and existing APIs
remain the source system.

## Delivered

- Typed `opencode.memory.graph.v1` Protobuf operations for prepare, upsert,
  run-status, search, status, paginated export, job enqueue/claim/renew/finish,
  job status, and cancellation.
- Domain schema generation 4 with explicit graph/job capabilities and actor routing.
- Actor-owned `knowledge-graph.json` plus one full-state bounded pending journal.
- NFKC/case/whitespace entity normalization scoped by project, memory scope,
  scope key, and entity type.
- Exact source evidence, native entity/relation IDs, seven allowed predicates,
  temporal assertion versions, run digests, and idempotent receipts.
- Source eligibility checks for visibility, pending deletion, expiry,
  supersession, stale code anchors, content hash, extraction revision, and
  egress-policy revision.
- Graph erasure integrated into ordinary/recovery upserts, supersession,
  delete/forget, expiry optimization, ingestion/index replacement, shared sync,
  import, and purge.
- Explicit OpenCode SDK v2 extraction in an isolated session with JSON Schema,
  denied permissions, disabled tools, bounded retries/timeouts/output, hook
  suppression, cleanup, permission prompting, and dry-run support.
- Current/as-of temporal search, pre-traversal eligibility filtering, bounded
  lexical/BFS expansion, source-visible status/export, and rank-only RRF
  projection into normal memory retrieval before MMR/context packing.
- Durable source-revision-bound extraction jobs with expiring worker leases,
  bounded retries/backoff, restart recovery, cooperative cancellation, outcome
  reconciliation, and atomic job-receipt/fact completion.
- `active-embedding.json` migration of the existing collection to an explicit
  local generation identity plus lexical fallback when dense inference is
  unavailable.
- Actor-owned daemon maintenance probes active projects every five minutes and
  runs optimize only for incomplete indexes, prunable expiry, or retrieval
  retention; expired deletion is batched at the native request limit.

## Deferred

- Reviewed graph observation/community summaries and LLM entity adjudication.
- Automatic graph extraction triggered by document indexing; provider egress
  remains explicit even though jobs are durable.
- Real target-generation model migration, worker memory admission, cutover,
  rollback, and remote/alternative embedding adapters. OpenCode's supported SDK
  does not expose a generic embedding API, so chat models are never treated as
  vector providers.
- Remote extraction for repository-scoped or code-backed sources.

## Design Record

Pressure: graph persistence must survive crashes and source deletion without a
second writer, database service, Python runtime, or duplicate source-of-truth.

Decision: keep a concrete actor-owned graph store inside `MemoryEngine`; use a
bounded full-state pending transaction and atomic replacement under the existing
project `writer.lock`.

Language form: Rust value types and closed enums, typed Protobuf boundaries, and
a narrow TypeScript OpenCode SDK adapter. No runtime graph-driver trait or
provider hierarchy is introduced because there is only one native backend.

Ownership: the project actor owns `MemoryEngine`, which owns zvec, lifecycle
state, document ownership, graph state, and the writer lock. The TypeScript
adapter owns only the temporary extraction session and submits untrusted
candidates; native code derives scopes and assigns IDs.

Alternatives: Graphiti/Python and PostgreSQL/Hindsight were rejected because
they create separate storage ownership and runtime dependencies. Lifecycle
relations were rejected because supersession/conflict semantics differ from
knowledge predicates. Calling provider credentials from Rust was rejected
because OpenCode owns provider auth.

Costs: whole-state JSON commits are bounded to 16 MiB and graph requests/jobs to
conservative candidate/traversal/page/lease limits. Graph-only requests currently
open the existing engine and therefore may incur model initialization. Graph RRF
uses ranked source IDs rather than combining heterogeneous raw scores.

Invariants: one writer; native IDs only; evidence required; same-scope facts;
idempotent run IDs; no source resurrection; graph reads require current visible
evidence; user deletion and purge durably remove evidence quotes before source
removal; provider credentials never cross into native state.
