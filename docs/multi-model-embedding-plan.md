# Multi-Model and Multimodal Embedding Plan

Status: Proposed

Research snapshot: 2026-07-27

Target: extend the shared memory daemon so one installation can use different
local Hugging Face models for text and image embeddings while returning model
RAM, Metal memory, and GPU memory when inference is idle.

Related plan: [Shared Daemon Migration Plan](./shared-daemon-migration-plan.md)

Single-model alternative:
[Qwen3-VL Single-Model Embedding Plan](./qwen3-vl-single-model-plan.md)

Project control plane:
[Project Embedding Model Switch Plan](./project-embedding-model-switch-plan.md)

## Decision Summary

1. Remove embedding model ownership from `MemoryEngine` and project actors.
2. Add a daemon-wide `ModelManager` that shares one model worker across every
   project using the same encoder identity.
3. Run resident models in daemon-owned child processes. A worker process exits
   after a configurable idle timeout so the operating system deterministically
   reclaims native allocator, mmap, Metal, and GPU resources.
4. Default the model idle timeout to 60 seconds. Support `0s` for immediate
   unload, arbitrary bounded durations, and `never` for users who prefer warm
   latency over idle memory.
5. Store vectors from incompatible models in separate embedding spaces and
   separate zvec collections. Equal dimensions do not imply compatible vectors.
6. Keep the existing Qwen3 4B text space for current collections. Add an
   official Qwen3 0.6B Q8 low-memory text profile, but do not silently re-embed
   existing data.
7. Limit the first multimodal release to text and raster images. Existing PDF,
   Markdown, and HTML ingestion continues to extract text through xberg. PDF
   page-image retrieval, OCR, audio, video, SVG, and multi-vector document
   retrieval are deferred.
8. Start with llama.cpp GGUF and ONNX dual-encoder runtime adapters. Evaluate
   Candle SigLIP2 as an optional quality profile after packaging and memory
   benchmarks.
9. Fuse results from different spaces by rank, not by comparing raw cosine
   scores from unrelated models.
10. Treat the daemon as the authority for model catalog resolution, artifact
    verification, resident-memory policy, and worker lifecycle.

## Why This Is a Separate Plan

The daemon migration first establishes one authoritative project-store owner
and fixes cross-process writer-lock conflicts. Its first release deliberately
allows one embedding context per active project actor and defers model sharing.

Multi-model support changes a different ownership boundary:

```text
Current daemon migration
  ProjectActor -> MemoryEngine -> LlamaCppEmbedder -> model/context

This plan
  ProjectActor -> MemoryEngine -> EmbeddingBroker
                                -> daemon ModelManager
                                -> shared model worker process
```

The multi-model implementation depends on stable daemon sessions, project
leases, actor serialization, cancellation, and idle shutdown. It must not
weaken the store-ownership guarantees from the daemon migration.

Two changes should still influence the daemon protocol before daemon v1 is
frozen:

- project acquisition must be able to select repeated embedding spaces rather
  than one singular embedding identity;
- daemon capabilities must advertise model-worker and embedding-space support.

## Goals

- Run multiple local embedding model families from Hugging Face artifacts.
- Route text and image inputs to compatible encoders.
- Share one resident encoder across different project actors.
- Keep daemon control and project storage responsive during model load and
  inference.
- Release model memory while OpenCode remains open but no embedding work is
  running.
- Preserve current text collections and require explicit reindexing for model
  changes.
- Keep all inference local and preserve the current local-first privacy model.
- Bound model downloads, decoded images, queues, CPU threads, resident memory,
  and concurrent model loads.
- Make model identity, preprocessing, vector-space compatibility, and runtime
  state observable.

## Non-Goals for the First Release

- A hosted or remote inference service.
- Executing arbitrary Python or Hugging Face `trust_remote_code` implementations.
- Generic support for every Transformers architecture.
- Mixing vectors from unrelated models because they have the same dimension.
- Visual page retrieval for PDFs.
- OCR or image caption generation.
- Audio or video embeddings.
- SVG rendering or embedding.
- Animated-image semantics.
- ColPali or ColQwen multi-vector indexing and MaxSim retrieval.
- Hot-reloading a daemon model catalog without a controlled daemon restart.
- Automatically migrating an existing collection to a cheaper model.

## Current Repository Constraints

### Model loading is part of project opening

`MemoryEngine::open()` currently performs this sequence:

```text
initialize zvec
  -> create private project/model directories
  -> acquire writer.lock
  -> load MemoryState
  -> load DocumentIndexManifest
  -> load LlamaCppEmbedder
  -> validate or create the zvec collection
  -> recover journals
```

The daemon's `ProjectActor::spawn()` calls `Service::initialize()`, which opens
the engine before publishing the actor as ready. Therefore `AcquireProject`
currently implies model load even when the client only needs status or lexical
search.

Relevant code:

- `src/engine/mod.rs::MemoryEngine::open`
- `src/daemon/actor.rs::ProjectActor::spawn`
- `src/rpc.rs::Service::initialize`

### Project leases prevent model release

The actor is idle only when its lease count and active command count are zero.
An OpenCode process normally keeps its project lease for the plugin lifetime,
so project actor eviction does not address model RAM while OpenCode is open but
inactive.

Model idleness must therefore be independent from:

- project leases;
- open zvec collections;
- writer-lock ownership;
- daemon session lifetime.

### The llama.cpp backend is process-global

The pinned `llama-cpp-2` wrapper allows `LlamaBackend::init()` only once per
process. A second initialization returns `BackendAlreadyInitialized` until the
first backend is dropped. The current daemon `model_load_lock` serializes model
loads but does not make multiple independent `LlamaBackend` instances valid.

This means the current one-embedder-per-project design is not a valid
multi-model foundation. Either one process-global backend must own every model,
or models must be isolated in different processes. This plan selects process
isolation because process exit also provides the strongest memory-reclamation
guarantee.

### Current context settings may amplify peak memory

The current embedder configures:

```text
n_ctx     = 8192
n_batch   = 8192
n_ubatch  = 8192
n_seq_max = 1
```

The upstream wrapper defaults are `n_batch=2048` and `n_ubatch=512`. The
current 8192-token physical microbatch may contribute materially to the
observed 7-8 GiB resident footprint, especially with the 4B model and full
Metal offload. Phase 0 must measure this rather than assume one exact cause.

Experiments must vary:

- context length: 2048, 4096, and 8192;
- logical batch size;
- physical microbatch size: 256, 512, and 1024;
- CPU-only versus Metal;
- model size and quantization;
- first inference versus steady-state inference;
- object drop versus worker-process exit.

### One collection currently means one vector space

The current zvec schema contains one fixed-dimension `embedding` field. The
project manifest records one model ID and one embedding dimension. This is a
valuable safety property and must be preserved per embedding space.

It is not valid to store these examples in one field:

```text
Qwen3 4B text vectors        2560 dimensions
Qwen3 0.6B text vectors      up to 1024 dimensions
Nomic text/image vectors      768 dimensions
CLIP text/image vectors       512 dimensions
```

Even if two models both emit 768 dimensions, their coordinates are not
comparable unless the models were explicitly trained into the same space.

### Current document ingestion is text-only

The native document path currently accepts PDF, Markdown, and HTML. xberg
extracts Markdown and OCR is disabled. Image ingestion needs a separate bounded
inspection and preprocessing path; it must not be treated as arbitrary xberg
text extraction.

### Current benchmark is a smoke test

`tests/benchmark/retrieval-v1` contains eight synthetic memories and six
queries. It is useful for detecting protocol and scoring regressions but is too
small to select a new default text model or calibrate a multimodal retriever.

## Research Findings

### Text model candidates

| Profile                         | Properties                                                                                              | Decision                                      |
| ------------------------------- | ------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| Current Qwen3-Embedding-4B GGUF | 4B parameters, 2560 dimensions, multilingual and code retrieval, current persisted space                | Keep as the quality and compatibility profile |
| Qwen3-Embedding-0.6B-GGUF Q8    | Official 639 MB Q8 artifact, Apache-2.0, 100+ languages, code retrieval, MRL from 32 to 1024 dimensions | Add as the low-memory candidate               |

The official Qwen3 0.6B GGUF repository currently publishes Q8 and F16
artifacts. The plan must not depend on an unreviewed community Q4 artifact just
to reduce another few hundred megabytes.

The 0.6B model is not a silent replacement for the 4B model. A different model,
quantization, or output dimension produces a new `EmbeddingSpaceId` and
requires reindexing.

### Image and cross-modal candidates

| Candidate                                     | Strengths                                                                                                              | Constraints                                                                                                    | Role                                                    |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| Nomic Embed Text v1.5 + Vision v1.5           | Shared 768-dimensional space, Apache-2.0, 92.9M vision parameters, FastEmbed support                                   | Primarily English                                                                                              | Integration baseline and optional English profile       |
| Multilingual CLIP text + CLIP ViT-B/32 vision | 512-dimensional shared space, text model is 0.1B, trained on 50+ languages including Vietnamese, Apache-2.0 text model | ONNX text export needs validated mean pooling and 768-to-512 projection; image artifact license must be locked | Product-default candidate for multilingual image search |
| Google SigLIP2 Base                           | Apache-2.0, multilingual, stronger modern vision-language encoder, Candle has a Rust SigLIP2 implementation            | About 0.4B F32 parameters and roughly 1.5 GB weights; larger package and runtime surface                       | Optional quality candidate after a Candle spike         |
| Jina CLIP v2                                  | Strong multilingual and multimodal quality, Matryoshka output                                                          | Downloadable model is CC BY-NC 4.0                                                                             | Do not ship as a default profile                        |
| ColQwen2.5                                    | Strong visual-document retrieval                                                                                       | 3B backbone, multi-vector output, MaxSim, research license                                                     | Out of scope                                            |

Phase 0 chooses the initial visual profile using the project's own English and
Vietnamese image-retrieval benchmark. The architecture does not depend on
which candidate wins.

### Runtime options

| Runtime                         | Fit                                                                                                                    | Decision                                                                                     |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| llama.cpp through `llama-cpp-2` | Existing GGUF text path, quantization, CPU, Metal, CUDA, Vulkan                                                        | Keep for text embedding workers                                                              |
| FastEmbed/ONNX Runtime          | Mature Rust text/image APIs, built-in Nomic and CLIP models, custom ONNX support, CPU and optional execution providers | Use for the first image-runtime spike                                                        |
| Candle                          | Native Rust safetensors, CPU, Accelerate, Metal, CUDA, current SigLIP2 implementation                                  | Add only if SigLIP2 wins the bake-off or ONNX packaging blocks the selected model            |
| EmbedAnything                   | Broad model and modality support                                                                                       | Large dependency surface and lower adoption than the selected primitives                     | Do not make it a core dependency |
| Transformers.js                 | Supports several quantized multimodal models                                                                           | Would place model execution in the plugin runtime and duplicate it across OpenCode processes | Reject for the daemon path       |

FastEmbed currently uses ONNX Runtime and supports image batching and custom
ONNX/preprocessor files. ONNX Runtime's CPU arena may retain allocations while
a session is alive. Worker-process exit avoids depending on every native
runtime returning all arenas to the daemon process.

## Target Architecture

```text
OpenCode process A                       OpenCode process B
       |                                        |
       +----------------+  +--------------------+
                        |  |
                        v  v
                  private daemon IPC
                        |
                        v
                User-level Rust daemon
                        |
          +-------------+----------------+
          |                              |
   ProjectRegistry                  ModelManager
          |                              |
   ProjectActor A --+                    +--> Encoder worker X
   ProjectActor B --+--> EmbeddingBroker +--> Encoder worker Y
          |                              +--> Encoder worker Z
   MemoryEngine/store
          |
   state + journal + zvec spaces
```

Ownership rules:

- A project actor owns one project store and its ordered mutation lane.
- The project actor never owns resident model weights or a runtime backend.
- `MemoryEngine` owns state and collections and receives an
  `EmbeddingBroker` handle.
- `ModelManager` owns model catalog resolution, worker deduplication, memory
  admission, idle timers, and worker status.
- A worker owns exactly one loaded encoder identity in the first release.
- Different projects may submit to one worker concurrently, but the worker
  uses a bounded serialized or explicitly batched inference queue.
- Model load and inference never run on Tokio core threads.
- Worker failure is isolated from the daemon and project actors.

## Identity Model

The implementation must not overload one model string for artifact identity,
vector compatibility, and runtime process reuse.

### EncoderId

`EncoderId` identifies the exact function that turns one supported input into a
vector:

```text
EncoderId
  = artifact content digests
  + runtime family and compatibility version
  + tokenizer or image preprocessor digest
  + input template or task prefix
  + pooling
  + optional projection digest
  + output truncation dimension
  + normalization
  + input modality
```

Examples:

```text
qwen3-06b-q8/text-query/1024/l2
qwen3-06b-q8/text-passage/1024/l2
nomic-v15/text-query/768/l2
nomic-v15/image-document/768/l2
```

Query and passage templates can differ while the encoders remain members of
the same embedding space.

### EmbeddingSpaceId

`EmbeddingSpaceId` identifies vectors that may be compared:

```text
EmbeddingSpaceId
  = alignment family
  + artifact family generation
  + output dimension
  + output transform
  + metric
```

For an aligned dual encoder:

```text
nomic-v1.5 text encoder -----+
                              +--> nomic-v1.5:768:l2:cosine
nomic-v1.5 image encoder ----+
```

An equal dimension is never sufficient to establish compatibility.

### RuntimeInstanceKey

`RuntimeInstanceKey` identifies a resident worker:

```text
RuntimeInstanceKey
  = EncoderId
  + execution provider
  + backend ABI
  + precision and quantization artifact
  + runtime-only context policy
```

Thread count, CPU permit count, and idle timeout are scheduling policy and do
not create a different persisted vector space. Changing the quantized weight
artifact does create a different encoder and requires reindexing.

## Model Catalog and Artifact Lock

The daemon ships a small reviewed built-in catalog and may load one explicit
user catalog. The catalog is declarative and cannot contain executable code.

Illustrative configuration:

```toml
schema_version = 1

[encoders.qwen3-06b-low-memory-query]
runtime = "llama_cpp_gguf_text"
repository = "Qwen/Qwen3-Embedding-0.6B-GGUF"
revision = "<immutable-commit>"
files = ["Qwen3-Embedding-0.6B-Q8_0.gguf"]
modality = "text"
role = "query"
pooling = "last"
dimension = 1024
normalize = true
space = "qwen3-06b-q8:1024:l2:cosine"
idle_timeout = "60s"

[encoders.visual-image]
runtime = "onnx_image"
repository = "<selected-vision-repository>"
revision = "<immutable-commit>"
files = ["model.onnx", "preprocessor_config.json"]
modality = "image"
role = "document"
dimension = 512
normalize = true
space = "multilingual-clip-v1:512:l2:cosine"
```

Catalog rules:

- A built-in profile pins an immutable repository commit.
- Every artifact is hashed after download.
- A generated model lock records repository, revision, filename, SHA-256,
  declared size, observed size, license, runtime adapter, and output contract.
- The content digest, not a mutable repository branch, decides reuse.
- A custom catalog can reference only supported runtime contracts.
- Unknown runtime names, output transforms, pooling methods, or modalities fail
  before download.
- A user may select a custom compatible Hugging Face model, but the daemon does
  not claim generic Transformers `AutoModel` behavior.

Proposed cache layout:

```text
<data-home>/opencode/memory/models/
  artifacts/<sha256>/model-file
  metadata/<encoder-id>.json
  model-catalog.lock.json
  downloads/*.partial
```

Downloads are atomic, size-bounded, owner-only, and performed by the daemon's
artifact manager on a bounded blocking worker. Model workers receive only
verified local artifacts and never receive `HF_TOKEN`.

## Model Worker Design

### Process shape

Use the existing native executable for an internal mode:

```text
opencode-memory --model-worker --runtime <runtime> --encoder <encoder-id>
```

The daemon spawns the process with a fixed argument array and sanitized
environment. It connects through framed Protobuf over child stdin/stdout.
Worker logs use stderr and never share the protocol channel.

The first internal worker protocol contains:

```text
WorkerHello
LoadEncoder
EmbedTextBatch
EmbedImageBatch
WorkerStatus
ShutdownWorker
WorkerResponse
WorkerFailure
```

Each request has an opaque ID, deadline, byte estimate, input count, expected
dimension, and expected `EmbeddingSpaceId`. A response repeats the encoder and
space identities and returns bounded vectors.

### Why a child process

An in-process registry is smaller but cannot provide the same idle-memory
guarantee across llama.cpp, ONNX Runtime, Metal, CUDA, and allocator versions.
Dropping a Rust session may leave runtime arenas or GPU caches attached to the
daemon process. Process exit provides:

- deterministic virtual-memory unmapping;
- deterministic native allocator teardown;
- deterministic Metal/CUDA context teardown;
- crash isolation;
- one independent llama.cpp backend per worker;
- a measurable process RSS and PID for diagnostics.

The extra IPC cost is small relative to local model inference, and vectors are
small compared with model weights.

### Request semantics

- Embedding computation is retry-safe because it has no durable side effect.
- A store or ingest operation computes its vector before writing the pending
  mutation journal.
- A worker crash before vector completion fails preparation and creates no
  storage mutation.
- A worker crash after replying cannot affect the already returned immutable
  vector.
- The project actor revalidates source hash, encoder identity, dimension, and
  space identity before journal admission.
- Image bytes, not a mutable path, are sent to the worker after parent-side
  path validation and hashing.
- Queues and total encoded bytes are bounded before allocation.

### Worker sharing

`ModelManager` keeps one registry entry per `RuntimeInstanceKey`:

```text
Unloaded
  -> ResolvingArtifacts
  -> StartingWorker
  -> Loading
  -> Ready
  -> CoolingDown
  -> Unloading
  -> Unloaded

Any state -> Failed -> bounded backoff -> Unloaded
```

Concurrent requests for an unloaded encoder await one opening attempt. They do
not spawn duplicate workers. Projects A and B using the same encoder submit to
the same bounded worker queue.

The first release serializes inference per worker unless the selected runtime's
batching API is explicitly exercised. Cross-project batching is a later
optimization and must preserve per-request deadlines and cancellation.

## Idle Memory and Resource Policy

### Idle timeout

Model idleness is independent from project actor idleness.

A model worker becomes idle when all of these are zero:

- active inference count;
- queued inference count;
- active model-load waiters;
- daemon-owned jobs holding a model permit;
- explicit administrative pins.

When the predicates reach zero, the supervisor starts a generation-tagged
timer. New work invalidates the timer. On expiry, the supervisor rechecks the
generation and counters, closes admission, drains no new work, asks the worker
to shut down, and waits for process exit.

Supported policy values:

| Value              | Behavior                                                          |
| ------------------ | ----------------------------------------------------------------- |
| `0s`               | Exit after every completed inference queue drain                  |
| `60s`              | Proposed default                                                  |
| Any valid duration | Keep warm for the configured duration                             |
| `never`            | Keep loaded until memory pressure, daemon drain, or manual unload |

The parser should support seconds, minutes, and hours and apply an explicit
upper bound for numeric durations. `never` is the only unbounded value.

### Memory admission

Before starting a worker, reserve:

```text
declared artifact resident estimate
  + runtime context estimate
  + maximum configured batch scratch estimate
  + safety margin
```

The first load records observed peak RSS and steady resident delta. Later loads
use the greater of declared and observed estimates.

If a load does not fit:

1. Evict idle workers by LRU.
2. Recompute available budget after each confirmed process exit.
3. Queue for a bounded duration if a compatible worker is currently unloading.
4. Return `RESOURCE_EXHAUSTED` with required, budget, resident, and retry-after
   diagnostics if no safe admission path exists.

The daemon must never intentionally start several multi-gigabyte loads and rely
on the operating system OOM killer.

### CPU and GPU policy

- Model load concurrency defaults to one.
- Aggregate embedding CPU permits are bounded daemon-wide.
- A worker receives an explicit thread count; it does not default every model
  to all cores.
- Image batch size defaults to one for desktop use.
- Metal/GPU inference defaults to one weighted permit until benchmarks prove a
  safe higher value.
- The selected execution provider and runtime ABI are reported in status.
- Memory pressure may evict an idle `never` worker unless an explicit hard pin
  policy is added later.

## Runtime Adapters

### llama_cpp_gguf_text

Responsibilities:

- load one verified GGUF artifact;
- own one `LlamaBackend`, model, and mutable context inside one worker process;
- implement query and passage templates;
- apply configured BOS/EOS behavior, pooling, MRL truncation, and L2
  normalization;
- enforce token and context bounds;
- report model tensor size, context settings, and observed RSS;
- support CPU and the already packaged optional hardware backends.

Phase 0 must determine safe `n_batch` and `n_ubatch` defaults. Reducing
microbatch size must not silently truncate input or change pooling semantics.
Long inputs require tested multi-batch context handling or deterministic
model-aware chunking.

### onnx_text and onnx_image

Responsibilities:

- load verified ONNX artifacts and tokenizer/preprocessor files;
- configure a bounded intra-op thread count;
- default to the CPU execution provider in the first portable release;
- optionally evaluate CoreML or other providers in a platform-specific spike;
- disable or bound memory-arena behavior where supported and beneficial;
- apply a reviewed set of pooling, layer-normalization, projection, truncation,
  and L2-normalization operations;
- reject unknown graph input/output contracts.

The multilingual CLIP text profile requires validation that its ONNX graph plus
mean pooling and the published 768-to-512 dense projection reproduces the
SentenceTransformers reference vectors within a fixed tolerance.

### candle_siglip

This adapter is optional. Candle currently contains a Rust SigLIP model and an
example that selects `google/siglip2-base-patch16-224`. The spike must prove:

- reference-compatible text and image features;
- CPU and Apple Silicon behavior;
- package size and build time;
- worker RSS and cold-start time;
- deterministic process-exit cleanup;
- no need for repository-provided executable code.

Do not add Candle to release packages unless SigLIP2 quality justifies the
extra runtime surface.

## Embedding Space Storage

### Layout

Keep the current collection in place and add optional spaces beside it:

```text
projects/<project-id>/
  writer.lock
  state.json
  document-index.json
  manifest.json                 # current/legacy text-space manifest
  zvec/                         # current/legacy text-space collection
  spaces/
    <space-id-hash>/
      manifest.json
      zvec/
```

Do not move the current `zvec/` directory during the first state migration. A
new space registry treats it as the legacy default text space.

### Space manifest

Each collection manifest records:

```text
space_manifest_version
embedding_space_id
dimension
metric
normalized
document_encoder_ids
query_encoder_ids
content_types
artifact digests
preprocessing digests
zvec version
created_at_ms
```

Opening a collection does not require loading its model. The manifest contains
the dimension and compatibility identity needed to open zvec. A new
user-defined space is created only after the first worker handshake confirms
the expected output contract.

### State schema

Increase the private state schema from v4 to v5 and add:

```text
embedding_space_id
content_type
optional asset metadata
```

The first release assigns each record to exactly one embedding space. A future
record may have linked representations in multiple spaces, but that requires a
separate journal and lifecycle design.

Legacy records receive the space identity derived from the current project
manifest during migration.

### Journal semantics

Pending upserts and deletes include `embedding_space_id`. Recovery opens the
referenced collection, validates its manifest, and replays idempotently before
finalizing state.

The operation order remains:

```text
prepare immutable vector
  -> validate source hash and space
  -> write pending journal
  -> write one zvec collection
  -> save state/document manifest
  -> clear pending journal
```

One record belongs to one space in v1, so no cross-collection atomic commit is
required.

## Text and Image Ingestion

### Text

- Manual memories, shared Markdown, and extracted document chunks use the
  selected text space.
- Existing PDF, Markdown, and HTML extraction remains unchanged initially.
- A text model change creates a new space and requires explicit reindexing.
- Model-aware token limits are checked before mutation journal admission.

### Images

The first release supports:

- `.png`;
- `.jpg` and `.jpeg`;
- `.webp`.

The first release rejects:

- SVG;
- animated GIF semantics;
- PDF page rendering;
- audio and video;
- arbitrary binary files.

Image validation must include:

- project-relative canonical path checks already used by document ingestion;
- regular-file and non-symlink checks;
- magic-byte MIME detection rather than extension-only trust;
- compressed byte limit;
- decoded width, height, and total-pixel limits;
- bounded RGB conversion and resize;
- content hash before worker submission;
- batch size and aggregate decoded-byte limits.

The parent daemon reads and hashes the bytes. The worker embeds those immutable
bytes rather than reopening a path that may have changed.

### Asset representation

Represent an image as a normal lifecycle-managed record with additive asset
metadata:

```text
title: deployment-diagram.png
content: Visual project asset at docs/deployment-diagram.png
source: asset:docs/deployment-diagram.png
content_type: image
embedding_space_id: multilingual-clip-v1:512:l2:cosine
asset:
  path: docs/deployment-diagram.png
  mime_type: image/png
  width: 1600
  height: 900
  content_hash: <sha256>
```

This preserves existing lifecycle, provenance, list, get, delete, export, and
code-anchor behavior. The tool result gives the agent a project-relative path
that existing file tools can inspect when needed.

Image content remains untrusted evidence. Retrieving an image record does not
turn image text or metadata into instructions.

### Automatic image indexing

Automatic repository-wide image indexing is disabled by default. Repositories
often contain thousands of icons, generated screenshots, fixtures, and vendor
assets. The first release supports:

- explicit `memory_ingest` for one image;
- reindex of images already owned by the document/asset manifest;
- optional configured globs in a later rollout step.

## Retrieval and Fusion

### Query routing

Default automatic recall remains text-only. This avoids loading a cross-modal
text tower for every chat message.

Image retrieval is selected explicitly through additive search inputs:

```json
{
  "query": "so do trien khai service",
  "content_types": ["memory", "document", "image"],
  "query_image_path": null,
  "spaces": ["auto"]
}
```

Rules:

- text memories use the configured text query encoder;
- text-to-image search uses the text tower aligned with the image space;
- image-to-image search uses the image query encoder for that space;
- lexical search runs over textual metadata where applicable;
- a request never encodes a Qwen query and compares it directly with CLIP or
  Nomic image vectors.

### Cross-space fusion

Raw cosine distributions differ by model and modality. Retrieve independently
and fuse ranked lists:

```text
query
  -> route selected spaces
  -> embed once per selected space
  -> top-N retrieval within each space
  -> per-space calibrated rank list
  -> weighted reciprocal-rank fusion
  -> merge by record ID
  -> lifecycle, scope, taxonomy, staleness, and feedback filters
  -> MMR and context-budget packing
```

Initial scoring rules:

- use weighted reciprocal ranks across spaces;
- treat lexical retrieval as another ranked channel;
- retain raw cosine only in a space-specific diagnostic/calibration field;
- do not feed an uncalibrated cross-model cosine into the current global
  logistic function;
- version the new score as `multispace_rrf_v1`;
- set abstention thresholds per space from frozen benchmark data;
- report which spaces were searched or skipped.

The text-only path for the same model must retain its existing score behavior
until a benchmarked scoring migration is intentionally enabled.

## Daemon Protocol Changes

The current daemon v1 draft carries one `EmbeddingIdentity` in
`AcquireProjectRequest`. Before that protocol is frozen, add repeated space
selection:

```proto
message EmbeddingSpaceSelection {
  string profile_id = 1;
  repeated string content_types = 2;
}

message AcquireProjectRequest {
  string daemon_instance_id = 1;
  string session_id = 2;
  string project_root = 3;
  string worktree = 4;
  optional string data_dir = 5;
  EmbeddingIdentity embedding = 6; // beta legacy/default-text bridge
  optional string model_cache = 7;
  repeated EmbeddingSpaceSelection embedding_spaces = 8;
  string model_catalog_digest = 9;
}
```

The singular field may bridge beta clients to one default text space. Once
removed, reserve field 6 rather than reuse it.

Add daemon capabilities:

```text
embedding_spaces_v1
model_workers_v1
model_idle_eviction_v1
image_embedding_v1
multispace_rrf_v1
```

Add project/status diagnostics without causing model load:

```text
configured spaces
open collections
encoder loaded/loading/unloaded/failed state
worker PID
execution provider
resident estimate and observed RSS
last inference time
idle deadline
cold load duration
queue depth
artifact digest
```

`status`, `doctor`, and daemon info must inspect registry/manifests only. They
must never initialize a worker as a side effect.

## Configuration Authority

Model runtime policy is daemon-wide. Different OpenCode processes cannot safely
start one shared daemon with conflicting resident budgets or artifact catalogs.

Proposed controls:

```text
OPENCODE_MEMORY_TEXT_PROFILE
OPENCODE_MEMORY_VISION_PROFILE
OPENCODE_MEMORY_MODEL_IDLE_TIMEOUT
OPENCODE_MEMORY_MODEL_RESIDENT_BUDGET
OPENCODE_MEMORY_MODEL_CATALOG
OPENCODE_MEMORY_MODEL_MAX_WORKERS
OPENCODE_MEMORY_MODEL_LOAD_TIMEOUT
```

Policy:

- the first daemon process loads and fingerprints its catalog and resource
  policy;
- clients send selected profile IDs and expected catalog digest;
- an incompatible live daemon reports its current digest and a controlled
  restart action;
- clients never push arbitrary executable model definitions over project RPC;
- changing daemon-wide policy initially requires idle drain and restart;
- project collection manifests remain authoritative for persisted vector
  compatibility.

Plugin options may select built-in profile IDs, but they do not redefine the
daemon's catalog or resident-memory policy.

## Security and Reliability

### Model supply chain

- Pin immutable Hugging Face commit revisions.
- Hash every artifact and persist a reviewed lock.
- Enforce artifact count and byte limits before download.
- Reject symlinked or non-regular cached artifacts.
- Never execute `trust_remote_code`, Python, shell scripts, or repository
  binaries.
- Do not deserialize arbitrary pickle weights.
- Prefer GGUF, ONNX, and safetensors artifacts.
- Record and review model licenses before shipping a built-in profile.
- Keep Hugging Face credentials in the daemon artifact resolver only.

### Worker isolation

- Spawn the packaged binary directly without a shell.
- Pass a fixed internal mode and validated identifiers.
- Sanitize inherited proxy, preload, dynamic-library, and token environment
  values as permitted by packaging requirements.
- Do not expose worker stdio to plugin clients.
- Bound worker frame size and decoded vector count.
- Terminate a worker that violates its output contract.
- Contain worker panic or crash and apply bounded restart backoff.
- On daemon drain, stop admission, finish or cancel safe preparation, and join
  every child process before endpoint removal.

### Image safety

- Enforce compressed and decoded size limits.
- Reject unsupported or ambiguous formats.
- Limit pixel count before full allocation where the decoder permits.
- Use one-frame semantics only for explicitly supported formats.
- Preserve source hash through preparation and commit.
- Treat decoded content and metadata as untrusted evidence.

## Migration and Compatibility

### Existing projects

Existing project manifests continue to identify their Qwen3 4B text model and
dimension. The first state migration:

1. Takes a recoverable state snapshot.
2. Reads the current root collection manifest.
3. Derives a legacy text `EmbeddingSpaceId` from the exact model and
   preprocessing identity.
4. Adds that space ID and `content_type=text` to every current record.
5. Writes state v5 atomically.
6. Leaves `zvec/` and `manifest.json` in place.

The migration does not download or load a model.

### Reindexing to a low-memory profile

Changing from Qwen3 4B to Qwen3 0.6B is explicit:

```text
validate target profile
  -> build a new space in a temporary directory
  -> batch re-embed canonical records
  -> verify record count and sample retrieval
  -> atomically switch the selected default space
  -> retain rollback metadata until user-approved cleanup
```

Do not mutate the existing collection in place. A failed or cancelled reindex
leaves the current collection authoritative.

### Sidecar rollback

The temporary sidecar rollback path from the daemon migration may continue to
support only the legacy single text model. Multi-space and image capabilities
are daemon-only. The client must fail clearly rather than silently ignore image
spaces when the sidecar transport is selected.

## Rollout Phases

### Phase 0: Memory and model feasibility spike

Deliverables:

- current daemon RSS breakdown for model, context, zvec, state, and runtime;
- Qwen3 4B context/microbatch matrix on CPU and Metal;
- official Qwen3 0.6B Q8 load, inference, and quality measurements;
- repeated llama worker load/exit/reload test;
- FastEmbed/ONNX Nomic reference-vector test;
- multilingual CLIP ONNX plus pooling/projection reference-vector test;
- Candle SigLIP2 reference-vector and packaging test;
- English/Vietnamese visual retrieval bake-off;
- native package size and startup matrix;
- selected first visual profile and runtime ADR.

Exit criteria:

- process exit releases at least 90% of model-attributable RSS within 10
  seconds;
- one worker can repeatedly load and exit without daemon instability;
- selected runtime works on macOS arm64 and Linux arm64/x64 build targets;
- selected model license is acceptable for package defaults;
- reference vectors match trusted implementation output within a locked
  tolerance;
- no arbitrary repository code is required.

### Phase 1: Model catalog and worker broker

Deliverables:

- `EncoderId`, `EmbeddingSpaceId`, and `RuntimeInstanceKey` types;
- built-in catalog and artifact lock format;
- content-addressed artifact resolver;
- internal worker Protobuf schema;
- daemon `ModelManager` and worker registry;
- llama.cpp text worker using the current model;
- bounded queues, deadlines, load backoff, and worker diagnostics.

Exit criteria:

- two projects using one encoder create one worker PID;
- the daemon no longer creates one `LlamaBackend` per project;
- `BackendAlreadyInitialized` cannot occur on the normal daemon path;
- worker crash does not crash the daemon;
- artifact mismatch fails before model load.

### Phase 2: Lazy loading and idle eviction

Deliverables:

- storage-only project open;
- no-model daemon/project status;
- configurable model idle timeout;
- generation-tagged unload timer;
- worker LRU and resident-memory admission;
- CPU/GPU/model-load permits;
- model status and structured metrics.

Exit criteria:

- daemon startup, session open, project acquisition, status, doctor, lexical
  search, and list load no model;
- an active project lease does not pin a model;
- the default worker exits after 60 seconds of model inactivity;
- `0s`, custom duration, and `never` behave deterministically;
- a request racing idle unload either reuses the ready worker or waits for one
  new worker, never two workers.

### Phase 3: Embedding-space storage

Deliverables:

- space registry and per-space manifests;
- collection map inside `MemoryEngine`;
- state v5 and migration/rollback;
- space-aware pending journals;
- legacy root collection adapter;
- space-aware get, list, delete, export, doctor, and optimize.

Exit criteria:

- current projects open without model load or collection movement;
- legacy records have one deterministic text space;
- two incompatible spaces cannot write to one collection;
- journal recovery routes to the correct collection;
- a failed migration restores the prior state.

### Phase 4: Image ingestion

Deliverables:

- image MIME and dimension inspection;
- decoded-pixel and byte limits;
- immutable image-byte worker requests;
- selected image encoder adapter;
- asset metadata and provenance;
- explicit image ingestion and re-ingestion;
- image-aware document/asset manifest ownership.

Exit criteria:

- supported images ingest into the selected visual space;
- unchanged images skip embedding;
- changed images replace their prior vector safely;
- deleted image records remove vector and lifecycle state consistently;
- malformed and oversized images fail without model or storage corruption;
- automatic repository-wide image indexing remains disabled by default.

### Phase 5: Multi-space retrieval

Deliverables:

- explicit content-type and image-query inputs;
- query routing by embedding space;
- cross-modal text and image query encoders;
- weighted reciprocal-rank fusion;
- per-space calibration and abstention;
- multi-space retrieval diagnostics and score version.

Exit criteria:

- Qwen vectors are never compared directly with visual-space vectors;
- text-to-image and image-to-image search pass the frozen visual benchmark;
- text-only automatic recall does not load a visual encoder;
- lexical-only search loads no encoder;
- text retrieval with the same model has no unexplained quality regression.

### Phase 6: Protocol and TypeScript integration

Deliverables:

- repeated space selection in daemon project acquisition;
- capability negotiation;
- generated Rust and TypeScript protocol updates;
- additive plugin search and ingest options;
- status rendering for model workers and spaces;
- client errors for unsupported sidecar/multimodal combinations.

Exit criteria:

- existing text tool calls remain source-compatible;
- old beta clients map to one default text space or fail with an explicit
  generation error;
- generated bindings and golden frames pass CI;
- one plugin process cannot alter global model policy for other clients.

### Phase 7: Packaging and guarded rollout

Deliverables:

- selected ONNX Runtime or Candle package dependencies;
- staged runtime libraries for every native package;
- third-party notices and built-in model license lock;
- package-size and cold-start reports;
- experimental multimodal flag;
- explicit low-memory reindex command;
- rollback and daemon-drain documentation.

Exit criteria:

- packaged workers load without development paths or network code execution;
- macOS arm64 and Linux arm64/x64 packages pass worker lifecycle tests;
- idle daemon memory meets the acceptance gate;
- existing projects are not automatically reindexed;
- multimodal can be disabled without making text memory unavailable.

## Test Plan

### Unit tests

- canonical `EncoderId` and `EmbeddingSpaceId` hashing;
- equal dimensions with different alignment families remain incompatible;
- model catalog schema and unknown runtime rejection;
- immutable revision and artifact digest validation;
- content-addressed cache paths and atomic downloads;
- idle-duration parsing for `0s`, seconds, minutes, hours, invalid values, and
  `never`;
- worker lifecycle state transitions and generation invalidation;
- memory reservation and LRU ordering;
- queue and byte accounting release after timeout, cancellation, crash, and
  normal completion;
- image MIME, dimension, pixel-count, and decompression-limit validation;
- legacy manifest to space-manifest conversion;
- state v4 to v5 migration and rollback;
- space-aware journal routing;
- rank fusion with model-incomparable cosine distributions;
- content-type routing that leaves visual spaces untouched by default recall.

### Runtime conformance tests

- Rust worker vectors versus trusted reference vectors;
- query and passage template conformance;
- MRL truncation followed by renormalization;
- Nomic text/image cross-modal similarity;
- multilingual CLIP Vietnamese text/image similarity;
- SigLIP2 text/image features if Candle is selected;
- CPU versus Metal numerical tolerance;
- repeated worker load, inference, exit, and reload;
- malformed worker frame and invalid vector dimension handling.

### Rust integration tests

- two project actors share one encoder worker;
- different encoders run in different bounded workers;
- project acquire and status load no worker;
- lexical search loads no worker;
- first dense search loads one worker;
- active project lease survives worker idle exit;
- next dense request cold-starts and succeeds;
- request racing unload creates no duplicate worker;
- worker crash during embedding creates no pending storage journal;
- daemon drain joins every worker process;
- state migration preserves current text retrieval;
- text and image collections coexist under one writer lock;
- crash recovery replays a space-aware pending mutation.

### TypeScript integration tests

- project acquisition sends selected profile IDs and catalog digest;
- unsupported capability returns an actionable error;
- existing memory search defaults to text content;
- explicit image content type reaches the visual space;
- image query path validation is preserved across IPC;
- model timeout configuration conflict reports controlled restart guidance;
- plugin disposal releases a project lease without terminating shared model
  work owned by another client.

### Black-box packaging tests

- packaged daemon spawns the packaged worker binary mode;
- worker resolves packaged llama.cpp, zvec, and ONNX/Candle runtime libraries;
- worker never depends on repository cwd;
- hostile proxy values cannot turn worker IPC into network traffic;
- absent Hugging Face credentials do not affect cached offline inference;
- worker PID disappears after idle timeout;
- daemon remains alive and low-memory after worker exit;
- native package verification includes model-worker mode and runtime libraries.

### Performance and memory tests

Measure:

- daemon no-model RSS;
- project-open RSS without inference;
- first model load peak RSS;
- steady model worker RSS;
- memory after Rust object drop;
- memory after worker process exit;
- CPU and Metal/unified-memory usage;
- cold and warm query p50/p95 latency;
- model load and artifact verification latency;
- throughput with one, two, and four projects sharing one encoder;
- contention with two different encoders;
- image decode and embedding peak bytes;
- queue wait and daemon scheduler responsiveness;
- package size per target.

## Benchmark Plan

### Text benchmark v2

Expand the frozen corpus to include:

- several hundred memories and queries;
- English and Vietnamese natural-language retrieval;
- source-code and identifier retrieval;
- long extracted document chunks;
- hard negatives and no-answer queries;
- current taxonomy and lifecycle filters.

Compare:

- current Qwen3 4B collection profile;
- Qwen3 0.6B Q8 at 1024 dimensions;
- Qwen3 0.6B Q8 at 768 dimensions;
- Qwen3 0.6B Q8 at 512 dimensions;
- context and microbatch policies selected by the memory spike.

Record Recall@1/3/5/10, MRR@10, nDCG@10, answerable coverage, false
abstention, no-answer specificity, cold/warm latency, peak RSS, steady RSS,
and vector-index size.

Do not change the default text profile for new stores until a reviewed artifact
locks acceptable quality deltas.

### Image benchmark v1

Include project-relevant images:

- architecture and sequence diagrams;
- UI and browser screenshots;
- terminal and error screenshots;
- charts and dashboards;
- documentation figures;
- photos and irrelevant hard negatives.

Include query categories:

- English text to image;
- Vietnamese text to image;
- image to image;
- filename-only lexical fallback;
- no-answer and ambiguous queries.

Compare Nomic, multilingual CLIP, and SigLIP2 when the runtime spike succeeds.
Record Recall@1/5/10, MRR@10, nDCG@10, false positives, cold/warm latency,
ingestion throughput, model RSS, model artifact size, and package delta.

Freeze fixture hashes and selected-model output tolerances in committed
manifests.

## Acceptance Criteria

The feature is complete when all of the following are true:

1. Daemon start, session open, project acquire, status, doctor, list, and
   lexical search load no model worker.
2. The default model idle timeout is 60 seconds and users can select `0s`, a
   custom duration, or `never`.
3. A model worker exits after its idle timeout even while OpenCode and its
   project lease remain active.
4. At least 90% of model-attributable RSS is released within 10 seconds of
   confirmed worker exit on the supported test platforms.
5. Idle daemon RSS remains within the measured no-model baseline plus a
   provisional 128 MiB allowance; Phase 0 locks the final platform-specific
   gate.
6. Multiple projects using the same `EncoderId` share one resident worker.
7. Different encoders never trigger `LlamaBackend::init()` conflicts.
8. Resident-memory admission prevents concurrent model loads from exceeding
   the configured budget.
9. A worker crash never crashes the daemon or leaves a model-preparation
   mutation journal.
10. Existing Qwen3 4B collections open and search without automatic
    re-embedding.
11. Changing model artifact, preprocessing, dimension, or space requires an
    explicit reindex.
12. Text and image vectors from incompatible spaces are stored separately and
    never compared directly.
13. Text-to-image and image-to-image retrieval pass the frozen selected-model
    benchmark.
14. Automatic text recall does not load a visual encoder by default.
15. Image ingestion enforces path, MIME, byte, dimension, and decoded-pixel
    limits.
16. Built-in model artifacts are pinned by immutable revision, digest, and
    reviewed license.
17. Model workers execute no Hugging Face repository code and receive no model
    download credential.
18. Supported native packages include and verify every required runtime
    library.
19. Disabling multimodal support leaves existing text memory fully functional.
20. Model and space status is observable without changing lifecycle state or
    loading an encoder.

## Risks and Mitigations

| Risk                                                    | Impact                                      | Mitigation                                                                     |
| ------------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------ |
| Worker cold start is noticeable                         | First dense request after idle is slower    | Configurable timeout, explicit warm profile, measured cold-load diagnostics    |
| Process IPC adds overhead                               | Small per-request latency increase          | Batch bounded inputs; vectors are small; benchmark against inference cost      |
| Current context settings cause most of the 7-8 GiB peak | Idle fix alone does not reduce active peak  | Phase 0 context/microbatch matrix and low-memory Qwen profile                  |
| ONNX Runtime increases package size                     | Larger native optional packages             | Package spike, dynamic/runtime staging review, add only selected providers     |
| Candle duplicates backend code                          | Build and package growth                    | Add only if SigLIP2 benchmark justifies it                                     |
| Multilingual CLIP ONNX omits projection                 | Incorrect 768-dimensional output            | Validate pooling and published 768-to-512 projection against reference vectors |
| Nomic underperforms Vietnamese queries                  | Poor cross-modal retrieval for target users | Multilingual benchmark and multilingual CLIP/SigLIP alternatives               |
| Equal-dimension models are mixed                        | Silent retrieval corruption                 | Explicit `EmbeddingSpaceId` alignment identity and per-space manifests         |
| Worker exits during new admission                       | Duplicate load or failed request            | Generation-tagged timer, atomic admission close, one opening future per key    |
| Model artifact changes upstream                         | Non-reproducible vectors                    | Immutable revisions, SHA-256 lock, content-addressed cache                     |
| Native runtime retains memory                           | Daemon remains multi-gigabyte when idle     | Worker-process exit rather than in-process-only drop                           |
| Automatic image indexing is expensive                   | Large downloads, CPU usage, noisy index     | Explicit ingestion first; auto image globs disabled by default                 |
| Image decompression bomb                                | Memory exhaustion before model admission    | Compressed-byte and decoded-pixel limits before inference                      |
| Reindex fails mid-migration                             | Existing retrieval unavailable              | Build new space beside old space and atomically switch only after verification |
| Sidecar rollback ignores new spaces                     | Missing image results                       | Declare multimodal daemon-only and fail explicitly on unsupported transport    |

## Files and Symbols to Change

### Rust model and daemon layers

- `src/embedding.rs`: retain or move the llama.cpp runtime implementation, but
  remove project-engine ownership.
- `src/model/mod.rs`: runtime-neutral identity and request types.
- `src/model/catalog.rs`: built-in/custom catalog and lock validation.
- `src/model/artifact.rs`: Hugging Face resolution, hashing, and
  content-addressed cache.
- `src/model/broker.rs`: project-facing embedding provider.
- `src/model/supervisor.rs`: worker registry, load deduplication, TTL, LRU, and
  memory admission.
- `src/model/worker.rs`: internal process mode and framed protocol.
- `src/model/runtime/llama_cpp.rs`: GGUF text adapter.
- `src/model/runtime/onnx.rs`: reviewed ONNX text/image contracts.
- `src/model/runtime/candle_siglip.rs`: optional post-spike adapter.
- `src/daemon/registry.rs`: replace `model_load_lock` with shared
  `ModelManager` ownership.
- `src/daemon/actor.rs`: inject an `EmbeddingBroker`; do not initialize a model
  while opening the actor.
- `src/daemon/mod.rs`: model policy, capability, worker status, and drain
  integration.
- `src/rpc.rs`: dispatch internal worker mode and preserve legacy sidecar
  behavior during beta.

### Rust engine and storage

- `src/engine/mod.rs`: open storage without a model; own a collection map and
  broker handle.
- `src/engine/retrieval.rs`: route spaces and add rank fusion.
- `src/storage/zvec.rs`: per-space path and manifest support.
- `src/storage/state.rs`: state v5, content type, asset metadata, and
  space-aware pending journals.
- `src/document.rs`: image inspection, MIME detection, byte/pixel limits, and
  immutable input preparation.
- `src/document_index.rs`: asset ownership and changed/deleted image handling.
- `src/config.rs`: catalog, profile, idle timeout, runtime budget, and worker
  configuration.
- `src/contract.rs`: additive content-type, asset, space, and status fields.

### Protocol and TypeScript

- `schema/opencode/memory/daemon/v1/daemon.proto`: repeated embedding-space
  selection and capabilities.
- `schema/opencode/memory/model/v1/worker.proto`: Rust-internal worker protocol.
- `schema/opencode/memory/v1/memory.proto`: additive image search/ingest and
  status values if the generic domain request needs them.
- `build.rs`: generate the worker schema and any selected runtime bindings.
- `opencode-memory/src/daemon-client.ts`: send profile selections and catalog
  digest.
- `opencode-memory/src/contracts.ts`: additive content type, asset, and model
  status types.
- `opencode-memory/src/plugin.ts`: image ingest/search arguments and profile
  options.
- `opencode-memory/src/protocol.ts`: generated domain/daemon values and golden
  frames.

### Packaging, tests, and documentation

- `Cargo.toml`: selected FastEmbed/ORT dependencies and optional Candle
  features.
- `scripts/stage-native.ts`: stage required runtime shared libraries.
- `scripts/verify-package.ts`: verify model-worker mode and libraries.
- `THIRD_PARTY_NOTICES.md` and `notices/`: runtime and model notices.
- `tests/`: daemon/model-worker lifecycle, migration, crash, and multi-space
  integration tests.
- `tests/benchmark/text-v2/`: expanded text corpus.
- `tests/benchmark/image-v1/`: visual corpus and relevance judgments.
- `.github/workflows/ci.yml`: worker packaging, memory lifecycle, and platform
  matrix.
- `README.md`: model profiles, idle timeout, image scope, migration, and
  resource expectations.
- `docs/shared-daemon-migration-plan.md`: add a short cross-reference after this
  plan is accepted; do not merge both implementation scopes.

## External References

### Models

- Qwen, [Qwen3-Embedding-0.6B](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)
- Qwen,
  [Qwen3-Embedding-0.6B-GGUF](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF)
- Nomic,
  [nomic-embed-vision-v1.5](https://huggingface.co/nomic-ai/nomic-embed-vision-v1.5)
- Nomic,
  [nomic-embed-text-v1.5](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5)
- Sentence Transformers,
  [CLIP ViT-B/32 multilingual text encoder](https://huggingface.co/sentence-transformers/clip-ViT-B-32-multilingual-v1)
- Google,
  [SigLIP2 Base Patch16 224](https://huggingface.co/google/siglip2-base-patch16-224)
- Jina AI, [Jina CLIP v2](https://huggingface.co/jinaai/jina-clip-v2)
- Vidore, [ColQwen2.5 v0.2](https://huggingface.co/vidore/colqwen2.5-v0.2)

### Runtimes

- UtilityAI,
  [`llama-cpp-2` backend lifecycle](https://github.com/utilityai/llama-cpp-rs/blob/main/llama-cpp-2/src/llama_backend.rs)
- FastEmbed Rust, [repository](https://github.com/Anush008/fastembed-rs)
- ONNX Runtime, [documentation](https://onnxruntime.ai/docs/)
- Hugging Face Candle, [repository](https://github.com/huggingface/candle)
- Candle,
  [SigLIP implementation](https://github.com/huggingface/candle/blob/main/candle-transformers/src/models/siglip.rs)

### Repository Evidence

- Current embedding load and context:
  [`src/embedding.rs`](../src/embedding.rs)
- Current project engine ownership:
  [`src/engine/mod.rs`](../src/engine/mod.rs)
- Current retrieval and calibration:
  [`src/engine/retrieval.rs`](../src/engine/retrieval.rs)
- Current daemon actor:
  [`src/daemon/actor.rs`](../src/daemon/actor.rs)
- Current daemon registry:
  [`src/daemon/registry.rs`](../src/daemon/registry.rs)
- Current zvec manifest/schema:
  [`src/storage/zvec.rs`](../src/storage/zvec.rs)
- Current document ingestion:
  [`src/document.rs`](../src/document.rs)
- Current daemon protocol draft:
  [`schema/opencode/memory/daemon/v1/daemon.proto`](../schema/opencode/memory/daemon/v1/daemon.proto)
- Current retrieval benchmark:
  [`tests/benchmark/retrieval-v1`](../tests/benchmark/retrieval-v1)
