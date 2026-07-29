# Embedding Engine Phase 1

Status: Implemented locally

## Decision

Pressure: embedding inference was serialized around complete project calls,
including lexical work, zvec writes, ranking, and state fsync. Bulk shared-memory
and document transactions also submitted one zvec upsert call per record. Model
selection remained a set of raw environment fields without a discoverable,
daemon-owned profile catalog.

Decision: keep llama.cpp inference globally serialized, but move the lock to the
actual query/passage embedding calls. Batch zvec upserts and shared-memory
commits without weakening the durable pending journal. Add an immutable catalog
of stable, preview, and unsupported profiles, and expose profile listing plus a
non-mutating switch preflight.

Language form: Rust value types and exhaustive Protobuf enum additions for the
closed profile-control methods; one shared TypeScript model-control facade for
the OpenCode tools.

Ownership: the daemon owns profile support decisions and preflight. Project
actors remain the only storage writers. `MemoryEngine` owns its collection and
embedder, while the daemon registry injects the shared inference lock. The
plugin never constructs model repository configurations.

Alternatives: changing model environment variables or rebuilding `zvec/` in
place was rejected because incompatible vectors could mix or make an existing
project unreadable. A public mutating switch was deferred because collection
generations, an active pointer, a durable switch journal, mutation freeze,
resume, cancellation, and rollback are not implemented yet.

Costs: profile metadata is duplicated from reviewed upstream model information
until an artifact-lock file is introduced. Preview profiles are visible but not
selectable. Actual model cutover remains a later generation-migration phase.

## Invariants

1. At most one llama.cpp embedding call runs at a time across project actors.
2. Lexical search, ranking, zvec flush, and state fsync do not hold inference
   capacity.
3. One pending upsert batch produces one zvec upsert call and one flush.
4. Shared-memory replacements are deleted only after successor writes commit.
5. Profile listing and dry-run preflight do not initialize `MemoryEngine` or
   load a model.
6. Only `qwen3-text-4b-q4` is selectable in phase 1.
7. Qwen3-VL profiles remain unsupported until runtime, artifact, quality,
   portability, and memory gates pass.
8. A non-dry-run switch is rejected before any project mutation.

## Built-In Profiles

| Profile                 | Support     | Runtime                        | Phase-1 behavior           |
| ----------------------- | ----------- | ------------------------------ | -------------------------- |
| `qwen3-text-4b-q4`      | Stable      | llama.cpp GGUF                 | Current/default profile    |
| `qwen3-text-0.6b-q8`    | Preview     | llama.cpp GGUF                 | Visible, preflight blocked |
| `qwen3-text-8b-q4`      | Preview     | llama.cpp GGUF                 | Visible, preflight blocked |
| `bge-m3`                | Preview     | Unvalidated                    | Visible, unsupported       |
| `nomic-embed-text-v1.5` | Preview     | Unvalidated                    | Visible, unsupported       |
| `qwen3-vl-embedding-2b` | Unsupported | No packaged multimodal runtime | Visible, unsupported       |
| `qwen3-vl-embedding-8b` | Unsupported | No packaged multimodal runtime | Visible, unsupported       |

The Qwen GGUF presets include pinned repository revisions and LFS SHA-256
digests. They remain non-selectable until retrieval-quality gates and the
generation migration are implemented.

The next phase adds legacy-root generation adaptation, managed generation
manifests, an atomic active pointer, and generation-aware pending journals
before any mutating switch command is enabled.

## Protobuf Boundary

The live memory wire contract remains in
`schema/opencode/memory/v1/memory.proto`; its released `Method`, `Request`, and
`Response` tags are unchanged. Model control is in
`schema/opencode/memory/model/v1/model.proto` with typed request/response
`oneof`s. `daemon.proto` adds new model branches without changing the existing
memory branch. Memory and model requests are validated as exactly one domain at
daemon admission, and the domain schema generation is incremented to `2` so an
older daemon cannot silently ignore a model branch.

The split follows the Protocol Buffers best-practice rules: new model messages
use fresh field numbers, old memory model-method numbers are reserved, enums
start with an `UNSPECIFIED` zero value, model operations use `oneof` rather than
an open method/payload pair, and optional fields preserve presence for nullable
profile metadata. The two domain files are intentionally grouped API surfaces
for this two-file packaging boundary; storage state remains represented by Rust
domain types and is not coupled to the RPC messages.
