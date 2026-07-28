# Qwen3-VL Single-Model Embedding Plan

Status: Proposed, blocked on runtime and memory feasibility

Research snapshot: 2026-07-28

Candidate: [`Qwen/Qwen3-VL-Embedding-8B`](https://huggingface.co/Qwen/Qwen3-VL-Embedding-8B)

Related plans:

- [Shared Daemon Migration Plan](./shared-daemon-migration-plan.md)
- [Multi-Model and Multimodal Embedding Plan](./multi-model-embedding-plan.md)
- [Project Embedding Model Switch Plan](./project-embedding-model-switch-plan.md)

This plan describes the restricted deployment mode where the production catalog
contains only the Qwen3-VL profile. If project-scoped profile switching is
enabled, the switch plan supersedes this document's one-resident-worker and
profile-acquisition assumptions. Its one-active-generation storage and unified
text/image retrieval rules still apply to a project while Qwen3-VL is active.

## Executive Decision

`Qwen3-VL-Embedding-8B` can functionally replace separate text and image
encoders. It maps text, images, screenshots, video, and mixed inputs into one
shared vector space. A text query can therefore retrieve text, images, or mixed
records from one zvec collection without cross-model score fusion.

That functional simplification does not make the model an immediate product
default for this local desktop application:

1. The official checkpoint contains 8,144,793,840 BF16 parameters and its four
   weight shards total about 16.29 GB. The complete Hugging Face repository is
   about 16.30 GB before runtime memory, activations, image processing, and
   framework overhead.
2. The official checkpoint is not GGUF and does not currently have a supported
   Rust, Candle, ONNX, Core ML, MLX, or upstream llama.cpp multimodal embedding
   path.
3. The documented runtimes are Sentence Transformers, Transformers/PyTorch,
   vLLM, and SGLang. These introduce Python and large native runtime
   dependencies; vLLM and SGLang primarily target GPU server deployments rather
   than a portable local desktop binary.
4. Upstream llama.cpp can run Qwen3-VL generation and text-only embedding
   models, but clean upstream does not yet provide the complete image-plus-text
   embedding path required by this checkpoint. Available GGUF and llama.cpp
   implementations are community conversions or forks.
5. The model is slightly weaker than the dedicated Qwen3 text embedders on
   Qwen's published MMTEB aggregate and does not explicitly claim the same
   programming-language and code-retrieval specialization.

The product decision is therefore:

- Design the multimodal feature around one active embedding model and one
  active dense space per project.
- Use `Qwen3-VL-Embedding-8B` as the Phase 0 quality candidate and reference
  implementation.
- Do not replace the current Qwen3 text model or ship the 8B model as the
  desktop default until runtime parity, package portability, text/code quality,
  cold-start, and active-memory gates pass.
- Preserve process isolation and idle worker exit. One model removes
  multi-model scheduling complexity, but process exit is still required to
  return its substantial resident memory reliably.
- If the 8B model fails the desktop memory gate, evaluate
  `Qwen3-VL-Embedding-2B` using the same architecture before returning to the
  separate text and vision model design.

If the hard requirement is specifically "use only the 8B model," the supported
product target must be described as a high-memory local or GPU profile, not as a
general laptop default, until measurements prove otherwise.

## What One Model Changes

The single-model design removes several parts of the earlier multi-model plan.

| Concern               | Multi-model design                               | Qwen3-VL single-model design                 |
| --------------------- | ------------------------------------------------ | -------------------------------------------- |
| Resident workers      | Registry of workers keyed by runtime and encoder | One supervised worker process                |
| Model runtimes        | llama.cpp plus ONNX and optional Candle          | One validated Qwen3-VL runtime               |
| Dense spaces          | Separate text and visual spaces                  | One active shared space                      |
| zvec collections      | One collection per embedding space               | One active collection generation per project |
| Cross-modal retrieval | Search visual space separately                   | Direct search in the shared space            |
| Dense result fusion   | Weighted reciprocal-rank fusion across spaces    | Not required                                 |
| Memory admission      | Per-model reservation and LRU                    | One worker reservation and one idle timer    |
| Project protocol      | Repeated embedding-space selection               | One immutable embedding profile              |
| Reindexing            | Per-space reindex jobs                           | One full-project generation switch           |

The following work is still required:

- decouple model loading from `MemoryEngine::open()` and project acquisition;
- run inference in a child process shared by all project actors;
- load lazily and exit after the configured idle timeout;
- validate immutable model artifacts and preprocessing code;
- support text, image, and mixed input requests;
- persist the exact model revision, dimension, prompt, preprocessing, and
  runtime identity;
- create an explicit migration and rollback path from the current text
  collection;
- extend state and zvec fields for image asset metadata and content-type
  filtering;
- benchmark text, code, image, screenshot, and Vietnamese retrieval;
- package or provision a runtime on macOS arm64 and Linux arm64/x64.

## Verified Model Facts

### Model contract

| Property                | Verified value                                              |
| ----------------------- | ----------------------------------------------------------- |
| Model type              | Multimodal embedding bi-encoder                             |
| License                 | Apache-2.0                                                  |
| Parameters              | 8,144,793,840                                               |
| Published weight dtype  | BF16                                                        |
| Inputs                  | Text, images, screenshots, videos, and mixed combinations   |
| Languages               | 30+; the official repository names 33, including Vietnamese |
| Context length          | 32K model limit                                             |
| Official helper default | 8192 tokens                                                 |
| Native output           | Up to 4096 dimensions                                       |
| MRL range               | User-selected dimensions from 64 to 4096                    |
| Pooling                 | Last-token representation followed by normalization         |
| Similarity              | Cosine, or dot product after L2 normalization               |
| Default instruction     | `Represent the user's input.`                               |

The model's text, image, and mixed outputs are intentionally trained into one
coordinate system. Unlike unrelated text and CLIP vectors, these values may be
compared directly when all inputs use the same checkpoint revision,
preprocessing, prompt policy, dimension, and normalization.

MRL dimension reduction takes a prefix of the full representation. The
truncated vector must be normalized again. Every query and stored vector in one
collection must use the same selected dimension.

The model card reports that task-specific English instructions usually improve
downstream performance by 1% to 5%. This is a vendor result, not a guarantee for
this project's corpus. The exact instruction policy must be benchmarked and
versioned as part of the embedding identity.

### Artifact size

The official repository publishes four BF16 safetensors shards:

| File                               | Approximate size |
| ---------------------------------- | ---------------: |
| `model-00001-of-00004.safetensors` |          5.00 GB |
| `model-00002-of-00004.safetensors` |          4.92 GB |
| `model-00003-of-00004.safetensors` |          4.92 GB |
| `model-00004-of-00004.safetensors` |          1.46 GB |
| Total weight shards                |         16.29 GB |
| Complete repository                |         16.30 GB |

This is approximately 15.18 GiB of repository storage. It is not a measured
resident-memory requirement. Active inference also needs framework state,
temporary tensors, decoded image buffers, attention workspaces, and possibly
KV cache or equivalent context state.

The model card's `Quantization Support` label refers to post-processing of the
output embedding. It does not mean Qwen publishes official INT8, FP8, AWQ,
GPTQ, GGUF, or MLX weight-quantized checkpoints for this model.

Community weight conversions exist, but none may become a built-in default
without independent artifact review, vector-parity tests, retrieval-quality
tests, and a license/provenance lock.

### Text quality tradeoff

Qwen's own MMTEB table reports:

| MMTEB metric          | Qwen3-VL 8B | Qwen3 text 4B | Qwen3 text 8B |
| --------------------- | ----------: | ------------: | ------------: |
| Mean by task          |       67.88 |         69.45 |         70.58 |
| Mean by type          |       58.88 |         60.86 |         61.69 |
| Retrieval             |       69.41 |         69.60 |         70.88 |
| STS                   |       75.41 |         80.86 |         81.08 |
| Instruction retrieval |        4.46 |         11.56 |         10.06 |
| Reranking             |       65.72 |         65.08 |         65.63 |

The VL model is nearly tied with the current 4B text family on the aggregate
retrieval column, but it is weaker on the overall text aggregate, STS, and
instruction retrieval. The dedicated Qwen3 text family also advertises 100+
natural and programming languages plus code retrieval; the VL model advertises
33 natural languages and does not make the same code-specific claim.

These published numbers do not answer whether opencode memory quality remains
acceptable. The project benchmark must include:

- source code and symbol descriptions;
- stack traces and error messages;
- Markdown decisions and short memory facts;
- English and Vietnamese queries;
- query-to-passage and passage-to-query asymmetry;
- screenshots containing code or UI state;
- diagrams and mixed text-plus-image memories.

### Visual quality tradeoff

The later Qwen model card reports these MMEB-V2 values:

| Metric                  | Qwen3-VL 2B | Qwen3-VL 8B | 8B gain |
| ----------------------- | ----------: | ----------: | ------: |
| Image classification    |        70.2 |        74.4 |    +4.2 |
| Image QA                |        74.4 |        81.0 |    +6.6 |
| Image retrieval         |        74.9 |        80.0 |    +5.1 |
| Image overall           |        75.0 |        80.1 |    +5.1 |
| Video overall           |        61.1 |        66.1 |    +5.0 |
| Visual-document overall |        80.2 |        83.3 |    +3.1 |
| All 78 datasets         |        73.4 |        77.9 |    +4.5 |

The 8B quality gain is meaningful if screenshot and visual-document retrieval
is a primary product feature. It must be weighed against an official 2B
repository of about 4.27 GB instead of 16.30 GB.

## Runtime Feasibility

### Official paths

Qwen documents these runtimes:

| Runtime               | Official usage                               | Fit for this project                                              |
| --------------------- | -------------------------------------------- | ----------------------------------------------------------------- |
| Sentence Transformers | Direct text, image, and mixed `encode` calls | Best correctness oracle; Python and PyTorch packaging burden      |
| Transformers/PyTorch  | Qwen helper and custom embedding behavior    | Best low-level reference; not a Rust-native deployment            |
| vLLM 0.14+            | Pooling runner and multimodal inputs         | Strong Linux GPU service candidate; poor embedded desktop fit     |
| SGLang                | Embedding engine and multimodal inputs       | GPU service candidate; version and platform matrix must be locked |

The official Python project currently requires Python 3.11+, PyTorch 2.8,
TorchVision, Transformers 4.57.3+, `qwen-vl-utils`, Accelerate, and image/video
dependencies. Vendoring that stack into the native npm package would add a
second package manager, large platform-specific wheels, and a much larger
security and support surface.

The model configuration uses a native Transformers Qwen3-VL architecture, but
correct embedding still depends on custom input formatting, multimodal
preprocessing, last-token extraction, truncation, and normalization behavior.
The integration must vendor or reimplement audited behavior rather than enable
arbitrary Hugging Face repository code.

### Current Rust path

The repository's current `LlamaCppEmbedder` accepts one GGUF file and exposes
only:

```text
embed_query(text)
embed_passage(text)
```

`Qwen3-VL-Embedding-8B` publishes safetensors shards, requires a visual
processor, and needs text, image, and mixed input support. It cannot be selected
by changing `OPENCODE_MEMORY_MODEL_REPO` and `OPENCODE_MEMORY_MODEL_FILE`.

The current code would reject the official artifact because
`resolve_model_path()` requires a `.gguf` filename. Even a community GGUF alone
is insufficient: the runtime also needs the correct vision projector,
multimodal position handling, prompt template, pooling, and embedding output
path.

### llama.cpp status

As of this research snapshot:

- clean upstream llama.cpp does not provide the complete Qwen3-VL multimodal
  embedding endpoint required by this model;
- upstream multimodal embedding pull requests were closed pending a more
  maintainable design or RFC;
- community GGUF repositories exist in several quantizations;
- a community patched llama.cpp fork exposes a dedicated
  `llama-vl-embedding` executable and reports high cosine parity on a small
  fixture;
- the community validation is not a broad retrieval benchmark and does not
  establish production portability or video parity;
- current `llama-cpp-2` bindings do not expose that community-only worker path.

The fork is useful for a feasibility spike, but it is not a production
dependency until all of these are true:

- the patch is upstreamed or intentionally vendored with an ownership plan;
- the Rust wrapper exposes the required multimodal API;
- official-reference vector parity passes for text, image, and mixed inputs;
- CPU, Metal, CUDA, and target Linux builds pass;
- malformed-image and worker-crash tests pass;
- the selected weight quantization passes retrieval-quality thresholds.

### Apple Silicon status

No official Qwen MLX or Core ML artifact was found for this checkpoint. The
vLLM Metal support table does not list Qwen3-VL embedding pooling as a supported
path. Community MLX ports exist, including mixed 8-bit conversions, but use
manual model and processor patches.

A community Apple implementation can be evaluated, but making it the macOS
runtime while Linux uses vLLM or llama.cpp would reintroduce multiple runtime
identities. Vectors must not share a collection until parity proves that the
platform runtimes implement the same preprocessing and embedding function
within a locked tolerance.

### Runtime decision gates

Phase 0 evaluates three paths in this order:

1. Official Sentence Transformers as the reference oracle only.
2. Community llama.cpp implementation as the preferred portable-native spike.
3. vLLM as the Linux GPU reference deployment.

The product implementation proceeds only when at least one deployable runtime
passes all target-platform and vector-parity gates. The official Python oracle
does not automatically qualify as the packaged runtime.

## Memory and Performance Policy

### Why one model does not solve active RAM

One worker prevents duplicate model copies across projects. It does not make an
8B BF16 model small.

The current application has observed roughly 7-8 GiB resident memory with its
4B GGUF path and large context/microbatch settings. The Qwen3-VL 8B official
weight payload alone is about 15.18 GiB. It is therefore unsafe to assume the
new model improves active RAM without weight quantization and a different
runtime.

The Phase 0 report must distinguish:

- disk artifact size;
- worker RSS before load;
- resident and virtual memory after load;
- first text embedding peak;
- first image embedding peak;
- steady-state text and image peaks;
- Metal or CUDA allocated and reserved memory;
- memory after request completion while the worker remains warm;
- memory after worker process exit;
- daemon RSS after worker exit.

### Provisional hardware matrix

These are test tiers, not vendor minimums:

| Tier                        | What Phase 0 establishes                                                    |
| --------------------------- | --------------------------------------------------------------------------- |
| 16 GB unified/system memory | Expected no-go for official BF16; verify graceful rejection rather than OOM |
| 24 GB unified/system memory | Determine whether constrained or quantized execution is viable              |
| 32 GB unified/system memory | Provisional minimum local 8B feasibility tier                               |
| 64 GB unified/system memory | Quality-profile reference tier                                              |
| 24 GB NVIDIA VRAM           | Test constrained BF16 or validated weight quantization                      |
| 32 GB+ accelerator memory   | Reference GPU quality tier                                                  |

No tier is declared supported until measured on the selected runtime. The
daemon must return `RESOURCE_EXHAUSTED` before spawning a model that exceeds its
configured budget. It must never rely on the operating-system OOM killer.

### Output dimension

Lowering the vector dimension reduces zvec storage and ANN work, not model
weight memory.

Phase 0 compares 512, 1024, 2048, and 4096 dimensions. For every reduced
dimension, the implementation must:

1. take the MRL prefix;
2. normalize the truncated vector;
3. use the same dimension for query and stored vectors;
4. assign a distinct embedding identity and collection generation;
5. measure text, code, Vietnamese, screenshot, and cross-modal quality.

The initial product dimension remains undecided until the benchmark. A 1024 or
2048 dimension profile is a plausible storage target, not a predetermined
default.

### Context and image bounds

The model advertises 32K context, while the official helper defaults to 8192.
The first release does not need 32K inputs because existing document ingestion
already chunks text.

Phase 0 starts with:

- 4096 or 8192 maximum text tokens;
- one input per image batch;
- one raster image per v1 memory representation;
- a conservative decoded-pixel limit below the helper's maximum;
- no video frames;
- no PDF page rasterization;
- bounded worker request bytes and execution deadline.

Larger values require separate peak-memory measurements.

## Target Architecture

```text
OpenCode plugin clients
          |
          v
Shared native memory daemon
  +-------------------+       +------------------------------+
  | ProjectRegistry   |       | EmbeddingWorkerSupervisor    |
  |                   |       |                              |
  | ProjectActor A ---+------>| one bounded request queue    |
  | ProjectActor B ---+------>| lazy start / health / idle   |
  | ProjectActor C ---+------>| crash backoff / process exit |
  +---------+---------+       +---------------+--------------+
            |                                 |
            v                                 v
  per-project state + zvec       one Qwen3-VL worker process
  one active generation          text | image | mixed input
```

### Ownership rules

- Project actors own writer locks, memory state, journals, and zvec
  collections.
- In the restricted single-model deployment, `EmbeddingWorkerSupervisor` is
  daemon-wide and owns one child process.
- The child owns all model/runtime objects and decoded inference tensors.
- No project actor owns or pins model memory.
- Each project has one persisted active embedding profile and output dimension.
  The restricted deployment selects Qwen3-VL for every project. The model-switch
  plan generalizes the supervisor when different projects select different
  profiles; project acquisition never performs the switch.
- The worker serves one bounded queue. The first release serializes inference
  unless the chosen runtime proves bounded concurrency safe.
- The worker never opens project stores or receives project writer-lock
  authority.
- The daemon validates files and sends immutable bytes or sealed temporary
  handles rather than mutable project paths.

### Why a child process remains required

Process isolation is still the preferred boundary even with one model:

- process exit returns allocator arenas, mmap state, Metal allocations, CUDA
  allocations, Python runtime state, and framework caches deterministically;
- a model runtime crash does not corrupt the daemon's project stores;
- model load does not block daemon status, session, or lexical operations;
- a future runtime can be changed without linking it into the authoritative
  storage process;
- resource limits and environment variables can be scoped to the worker.

## Embedding Identity

One model still needs an exact, immutable identity. `model_id` plus vector
dimension is not sufficient.

```text
EmbeddingProfileId = hash(
  repository,
  revision,
  artifact_digests,
  runtime_family,
  runtime_version,
  preprocessing_version,
  chat_template_digest,
  instruction_policy,
  pooling,
  normalization,
  output_dimension,
  output_precision,
  image_resize_policy,
  image_normalization_policy
)
```

The profile identity is persisted in every collection generation manifest and
returned by worker handshake. The daemon compares the complete identity before
accepting any vectors.

Changing any vector-affecting field requires a new collection generation and
explicit reindex. CPU, Metal, and CUDA may share one identity only after runtime
conformance tests show vectors remain within the locked numerical tolerance and
retrieval results remain equivalent.

## Worker Design

### State machine

```text
Stopped -> Starting -> Loading -> Ready -> Idle -> Stopping -> Stopped
                         |         |        |
                         +-------> Failed <-+
```

The supervisor tracks a generation number. Idle timers, process-exit events,
and in-flight responses must match the current generation before mutating
state. This prevents a stale timer or old worker exit from stopping a newly
started worker.

### Request types

The internal framed Protobuf protocol needs:

```text
Hello
LoadProfile
EmbedBatch
Health
Shutdown
```

Each `EmbedBatch` item contains exactly one of:

```text
TextInput
ImageInput
MixedInput { text, image }
```

Each request also contains:

- request ID and deadline;
- instruction policy ID;
- expected output dimension;
- expected profile ID;
- bounded image bytes and declared MIME type;
- cancellation token or call ID;
- maximum result bytes.

The worker response includes:

- profile ID;
- output dimension;
- normalized `f32` vector;
- input-token and image-shape diagnostics;
- runtime and device diagnostics;
- bounded timing fields.

The daemon rejects non-finite values, wrong dimensions, non-normalized output,
wrong profile IDs, oversized frames, and responses after cancellation or
deadline expiry.

### Lazy load and idle exit

The worker starts only for operations requiring a dense vector:

- memory store or update;
- dense or hybrid text query;
- explicit image ingestion;
- explicit image or mixed query;
- controlled reindex.

These operations must not start the worker:

- daemon startup;
- `GetDaemonInfo`;
- session open and heartbeat;
- project acquisition;
- status and doctor without a deep model probe;
- list, get, export, and lexical-only search;
- delete operations that do not require re-embedding.

The model is idle when there are no active requests, queued requests, load
operations, or reindex leases. The default idle timeout remains 60 seconds,
with `0s`, custom bounded durations, and `never` supported.

An idle `never` worker may still be terminated under explicit memory pressure
unless the user sets a separate hard pin. `never` must not bypass a global
resident-memory safety limit.

## Storage Design

### One active dense generation

The single-model design does not need a general map of unrelated spaces.
Every project has exactly one active dense collection generation.

```text
projects/<project-id>/
  state.json
  document-index.json
  pending/
  manifest.json                 # legacy/current generation before migration
  zvec/                         # legacy/current collection before migration
  embedding-generations/
    <profile-id-hash>/
      manifest.json
      zvec/
  active-embedding.json         # installed atomically after complete reindex
```

The root `manifest.json` and `zvec/` remain untouched until migration succeeds.
The new generation is built beside them. `active-embedding.json` selects one
generation atomically. A failed reindex leaves the old generation active.

Only one generation receives new writes. An old generation may remain on disk
for explicit rollback until cleanup. It is not searched or updated in normal
operation.

### Manifest

The generation manifest records:

```text
schema_version
project_id
embedding_profile_id
repository
revision
artifact_digests
runtime_family
runtime_version
preprocessing_version
instruction_policy
pooling
normalization
embedding_dimension
output_precision
supported_content_types
created_at_ms
completed_at_ms
record_count
source_state_generation
```

The collection opens only when its schema dimension and complete profile ID
match the daemon profile.

### Record representation

Each memory record has one dense representation in the active generation.

Text memory:

```text
TextInput(content)
```

Image memory without annotation:

```text
ImageInput(image_bytes)
```

Image memory with user text, title, or extracted trusted metadata:

```text
MixedInput(text, image_bytes)
```

The state and zvec metadata add:

- `content_type`: `text`, `image`, or `mixed`;
- source path or asset ID;
- content hash;
- image MIME type;
- original byte length;
- decoded width and height;
- optional text annotation;
- preprocessing version;
- embedding profile ID.

One image and its annotation should normally produce one mixed vector, not
independent unrelated records. Multiple document pages or screenshots remain
separate records so retrieval can return the relevant asset.

## Ingestion

### Text

Existing PDF, Markdown, and HTML extraction remains text-first:

```text
source document
  -> xberg text extraction
  -> current chunking
  -> TextInput per chunk
  -> one Qwen3-VL vector per chunk
```

PDF page rendering and visual-page indexing remain deferred. The model can
encode screenshots, but it does not remove the need for a safe PDF renderer,
page ownership, and lifecycle tracking.

### Images

The first image release accepts explicit `.png`, `.jpg`, `.jpeg`, and `.webp`
files. Automatic repository-wide image indexing remains disabled by default.

The daemon performs all validation before worker submission:

- canonicalize and authorize the project-relative path;
- open and read a bounded number of bytes;
- identify MIME from content rather than extension alone;
- reject animation unless one-frame semantics are explicitly defined;
- decode dimensions with a bounded parser;
- enforce compressed-byte, width, height, and total-pixel limits;
- reject decompression bombs and malformed metadata;
- hash the exact immutable bytes sent to the worker;
- remove metadata not used by the locked preprocessing contract.

The worker does not fetch URLs or reopen mutable project paths. Official Qwen
examples accept URLs for convenience, but the product runtime accepts only
daemon-supplied immutable local content.

### Video and audio

The model supports video, but video ingestion remains out of scope for v1 due
to decoder dependencies, frame sampling policy, duration limits, high peak
memory, and lifecycle complexity. Audio is not a supported model modality.

## Retrieval

### Dense retrieval

All content types live in one compatible dense collection:

```text
text query  -> text vector  -> text | image | mixed results
image query -> image vector -> text | image | mixed results
mixed query -> mixed vector -> text | image | mixed results
```

The request may filter `content_types`. Automatic memory recall remains text
query only, but it may return image or mixed memories when visual recall is
enabled by the user. It must not read or return raw image bytes unless the tool
contract explicitly requests them.

### Hybrid retrieval

Lexical search applies to text content and the text component of mixed records.
Image-only records have no fabricated OCR text.

For text queries:

```text
dense shared-space results + lexical text results -> existing hybrid fusion
```

For image queries:

```text
dense shared-space results only
```

No cross-model RRF is needed because there is one dense space. The score
version still changes because model, prompt, dimension, and content-type
behavior change.

### Instruction policy

Phase 0 compares at least:

- Qwen's default `Represent the user's input.`;
- one retrieval-specific English instruction for queries;
- symmetric instruction on query and records;
- asymmetric query instruction with neutral record encoding.

The winning policy is immutable within one profile. Changing it requires a new
generation and reindex.

## Daemon and Public Protocol

### Project acquisition

The current singular `EmbeddingIdentity embedding = 6` remains conceptually
correct for one active project profile, but project acquisition must not be the
mutation that changes it. The persisted project profile is authoritative after
first initialization; profile changes use the durable model-switch operation.

Before protocol freeze, extend or replace fields so identity includes:

```proto
message EmbeddingIdentity {
  string profile_id = 1;
  string repository = 2;
  string revision = 3;
  string artifact_set_digest = 4;
  string runtime_family = 5;
  string runtime_version = 6;
  string preprocessing_version = 7;
  string instruction_policy = 8;
  string pooling = 9;
  bool normalize = 10;
  uint32 dimension = 11;
  string output_precision = 12;
  repeated string input_modalities = 13;
}
```

The field numbers are illustrative. Existing beta field numbers must be
reserved or migrated deliberately rather than reused accidentally.

For an existing project, acquisition either omits a profile expectation or
checks an optional expected profile without mutating it. A mismatch returns the
persisted active profile and an actionable error. Clients may request
capabilities, but they do not provide arbitrary model repositories or enable
remote code.

### Tool requests

The domain protocol adds additive input fields for:

- `content_type` on store and ingest;
- `image_path` or a daemon-authorized asset reference;
- optional image text annotation;
- `query_image_path` for explicit image search;
- requested `content_types` filters;
- dense/lexical/hybrid search mode;
- response asset metadata without automatic raw image transfer.

The TypeScript client sends local paths only. The daemon resolves, authorizes,
reads, validates, and hashes the bytes.

### Capabilities

Daemon capabilities include:

```text
embedding_worker
embedding_worker_idle_exit
unified_multimodal_space
image_ingest
image_query
mixed_input_embedding
embedding_generation_migration
```

Capabilities are advertised only when the packaged runtime and model profile
are actually available on the current platform.

## Configuration

The daemon owns the global profile catalog and runtime policy. Each project
persists one selected profile from that catalog:

```text
OPENCODE_MEMORY_EMBEDDING_PROFILE=qwen3-vl-embedding-8b
OPENCODE_MEMORY_MODEL_IDLE_TIMEOUT=60s
OPENCODE_MEMORY_MODEL_MEMORY_BUDGET_BYTES=<auto-or-explicit>
OPENCODE_MEMORY_MODEL_DEVICE=auto|cpu|metal|cuda
OPENCODE_MEMORY_EMBEDDING_DIMENSION=<benchmarked-value>
OPENCODE_MEMORY_MODEL_MAX_CONTEXT=8192
OPENCODE_MEMORY_MODEL_MAX_IMAGE_PIXELS=<bounded-value>
OPENCODE_MEMORY_MODEL_MAX_INPUT_BYTES=<bounded-value>
```

Repository, revision, artifact hashes, preprocessing version, prompt policy,
and runtime version come from the built-in signed or release-locked profile.
They are not free-form user environment variables in the production profile.

Advanced local model overrides may exist for development, but they create a
different profile identity and receive no compatibility guarantee.

## Security

- Never execute arbitrary code downloaded from a model repository.
- Pin the Qwen model revision and every artifact SHA-256.
- Vendor and review any required preprocessing or embedding helper code.
- Do not pass `trust_remote_code=True` against an unpinned mutable repository.
- Do not allow the worker to fetch images, model files, or URLs after startup.
- Give the worker read-only access only to its content-addressed model cache.
- Keep project stores and writer locks inaccessible to the worker.
- Bound all frames, vectors, text, image bytes, decoded pixels, queues, and
  deadlines.
- Validate output dimensions, finiteness, normalization, and profile identity.
- Treat community GGUF, MLX, INT8, and FP8 conversions as untrusted until
  reviewed and rehashed into the release catalog.
- Record third-party notices for the model, runtime, image decoder, and every
  bundled native library.

## Migration and Rollback

The current Qwen3 text collection is not vector-compatible with Qwen3-VL. A
model switch always requires reindexing.

Migration sequence:

1. Open the current project without loading either model.
2. Verify state, journals, document ownership, and legacy manifest.
3. Resolve and verify the Qwen3-VL profile artifacts.
4. Create a new generation directory with an incomplete manifest.
5. Re-embed text memories and document chunks from authoritative state.
6. Re-embed explicitly registered images and mixed records.
7. Flush zvec and validate record count, dimensions, finite values, and sample
   search probes.
8. Mark the generation complete and fsync it.
9. Atomically replace `active-embedding.json`.
10. Keep the legacy generation read-only for a bounded rollback window.
11. Delete the old generation only after explicit cleanup or retention expiry.

Writes arriving during reindex use a bounded change journal. Before the active
pointer switch, replay changes into the new generation and verify that its
source state generation matches current state.

Rollback changes only the active generation pointer and daemon profile. It does
not reinterpret Qwen3-VL vectors as Qwen3 text vectors.

## Rollout Phases

### Phase 0: Feasibility and reference oracle

Deliverables:

- pin the official model revision and artifact hashes;
- run the official Sentence Transformers implementation as the oracle;
- freeze text, code, Vietnamese, image, screenshot, and mixed-input fixtures;
- record vectors at 512, 1024, 2048, and 4096 dimensions;
- measure official BF16 memory, cold start, throughput, and idle behavior;
- evaluate the community llama.cpp path and vLLM against oracle vectors;
- measure 16, 24, 32, and 64 GB system tiers where hardware is available;
- produce package-size and runtime-dependency reports.

Exit criteria:

- one deployable runtime supports text, image, and mixed input;
- text and image vectors match the oracle within locked tolerances;
- project retrieval quality meets agreed gates;
- worker exit returns at least 90% of model-attributable RSS within 10 seconds;
- low-memory machines fail admission before load rather than OOM;
- runtime works on macOS arm64 and Linux arm64/x64, or the product explicitly
  narrows its support matrix;
- no arbitrary repository code execution is required.

Failure action:

- evaluate Qwen3-VL-Embedding-2B with the same fixtures and architecture;
- if 2B also fails runtime or quality gates, retain the multi-model plan.

### Phase 1: Single worker supervisor

Deliverables:

- daemon-wide `EmbeddingWorkerSupervisor`;
- storage-only `MemoryEngine::open()`;
- lazy worker start and 60-second idle exit;
- framed internal Protobuf protocol;
- one bounded request queue;
- crash restart, backoff, deadline, and cancellation handling;
- model status without forcing load.

Exit criteria:

- acquiring any number of projects creates no model process;
- first dense request starts exactly one worker;
- all projects share that worker;
- lexical-only and status operations load no model;
- worker crash does not terminate the daemon or corrupt journals;
- idle exit releases model-attributable memory.

### Phase 2: Unified input adapter

Deliverables:

- text, image, and mixed worker requests;
- locked Qwen prompt and preprocessing behavior;
- image byte and decoded-pixel validation;
- MRL prefix truncation and re-normalization;
- runtime conformance fixtures.

Exit criteria:

- official-reference vectors pass for all three input forms;
- malformed and oversized images fail before inference;
- the worker performs no network access;
- image batch size one has bounded peak memory;
- selected output dimension passes quality and storage gates.

### Phase 3: Collection generations and migration

Deliverables:

- active generation manifest and pointer;
- non-destructive legacy migration;
- image and mixed record metadata;
- reindex journal and atomic pointer switch;
- rollback and cleanup commands.

Exit criteria:

- existing projects open without model load;
- migration never mutates the legacy collection in place;
- failed or cancelled reindex leaves the old generation active;
- one and only one generation receives writes;
- rollback restores the prior profile and search behavior.

### Phase 4: Retrieval and plugin integration

Deliverables:

- text-to-any, image-to-any, and mixed-to-any dense search;
- content-type filters;
- lexical behavior for text and mixed records;
- additive Rust and TypeScript protocol fields;
- capability negotiation and model diagnostics;
- explicit image ingest and search tool inputs.

Exit criteria:

- no cross-model dense fusion exists on the normal path;
- text queries retrieve relevant images and vice versa;
- automatic recall remains bounded and opt-in for visual results;
- existing text-only tool calls remain source-compatible;
- unsupported clients receive explicit capability errors.

### Phase 5: Packaging and guarded release

Deliverables:

- target-platform worker artifacts;
- model/runtime notices and artifact lock;
- memory admission defaults by platform;
- experimental feature flag;
- download, migration, rollback, and cleanup UX;
- black-box npm package tests.

Exit criteria:

- packaged inference does not depend on development paths;
- macOS arm64 and Linux arm64/x64 pass the declared support matrix;
- fresh install, offline restart, upgrade, and uninstall behavior pass;
- 8B is not enabled automatically on unsupported memory tiers;
- disabling multimodal mode leaves existing text memory available.

## Test Plan

### Model conformance

- exact official revision and artifact digests;
- text, image, and mixed vectors versus Sentence Transformers oracle;
- last-token pooling and L2 normalization;
- MRL prefix and re-normalization at every candidate dimension;
- default and selected instruction policies;
- CPU, Metal, CUDA, and runtime-version parity;
- deterministic behavior within numerical tolerance;
- community weight-quantization quality if considered.

### Retrieval quality

- text memory retrieval;
- source-code and symbol retrieval;
- stack-trace and error-message retrieval;
- English and Vietnamese queries;
- screenshot-to-text and text-to-screenshot retrieval;
- diagram and UI retrieval;
- image-to-image retrieval;
- mixed text-plus-image retrieval;
- hard negatives with visually similar but semantically different images;
- dimension and output-vector quantization ablations.

### Worker lifecycle

- no load during daemon, session, project, status, doctor, list, or lexical
  paths;
- one worker across concurrent projects;
- duplicate first requests create one process;
- request arriving during idle shutdown starts or reuses exactly one process;
- timeout and cancellation release queue and active counters;
- crash during load, text inference, image decode, and response write;
- repeated load, infer, idle exit, and reload;
- daemon shutdown terminates the worker and descendants;
- stale generation events cannot stop a replacement worker.

### Storage and migration

- profile mismatch rejection;
- fixed dimension enforcement;
- content-type field round trips;
- text, image, and mixed record lifecycle;
- reindex with concurrent writes and deletes;
- cancellation before and after active pointer switch;
- journal recovery routes to the selected generation;
- rollback and cleanup preserve authoritative state;
- old projects remain readable before migration.

### Security

- artifact digest mismatch;
- mutable revision rejection;
- worker network denial;
- path traversal and symlink races;
- MIME spoofing and malformed images;
- decompression bombs and pixel-limit violations;
- oversized Protobuf frames and vectors;
- non-finite and non-normalized worker output;
- untrusted runtime/profile selection attempts;
- worker access attempts against project stores.

### Performance

- daemon baseline RSS with no worker;
- BF16 and candidate quantized worker RSS;
- text and image cold-start latency;
- first-request and steady-state latency;
- text and image indexing throughput;
- context, image size, dimension, and batch-size sweeps;
- memory after request completion and after process exit;
- zvec size and recall at each output dimension;
- concurrent project fairness through one queue.

## Acceptance Criteria

The Qwen3-VL 8B single-model feature is ready only when:

1. One checkpoint and one profile serve text, image, and mixed input.
2. Every project has exactly one active dense generation.
3. Text and image vectors are searched directly without unrelated-space score
   fusion.
4. Project acquisition and status do not load the model.
5. All projects share exactly one worker process.
6. The default idle worker exits after 60 seconds of model inactivity.
7. Worker exit releases at least 90% of model-attributable RSS within 10
   seconds on each supported platform.
8. The daemon remains within baseline plus 128 MiB after worker exit, subject to
   Phase 0 adjustment for measured non-model runtime overhead.
9. Memory admission rejects unsupported hardware before model load.
10. No arbitrary Hugging Face repository code executes.
11. Official artifact and preprocessing revisions are immutable and hashed.
12. Packaged runtime vectors pass the official-reference conformance suite.
13. Selected output dimension passes the frozen quality benchmark.
14. Text and code retrieval do not regress beyond the agreed threshold.
15. Vietnamese text and cross-modal fixtures pass.
16. Image and mixed ingestion enforce byte, dimension, pixel, and MIME limits.
17. The worker performs no network or project-store access.
18. Existing collections are never silently reinterpreted or re-embedded.
19. Migration is non-destructive and rollback is atomic.
20. macOS arm64 and Linux arm64/x64 pass the declared support matrix.
21. Package size, model download size, cold start, and active memory are shown
    before the user enables the 8B profile.
22. Unsupported low-memory machines fail clearly rather than swap heavily or
    OOM.

## Files Expected to Change

### Rust

- `src/embedding.rs`: replace the text-only engine-facing trait with brokered
  text, image, and mixed requests; keep llama.cpp implementation only for the
  legacy profile during migration.
- `src/engine/mod.rs`: open storage without loading an embedder; use one active
  collection generation.
- `src/engine/retrieval.rs`: route text, image, and mixed queries through the
  shared space and content-type filters.
- `src/daemon/registry.rs`: remove model ownership and inference locks from
  project actors; inject one supervisor.
- `src/daemon/actor.rs`: call the broker without tying model idleness to project
  leases.
- `src/daemon/`: add worker supervisor, framed worker client, process lifecycle,
  admission, diagnostics, and internal protocol.
- `src/storage/zvec.rs`: add generation manifests, active pointer selection,
  content type, and asset fields while preserving the legacy root collection.
- `src/document.rs`: add bounded raster-image inspection without changing text
  extraction semantics.
- `src/document_index.rs`: track image and mixed asset ownership.
- `src/state.rs`: add content type, asset metadata, profile identity, and
  migration generation.
- `src/config.rs`: add one locked profile, idle timeout, memory budget, device,
  context, image, and dimension settings.
- `src/contract.rs`: add image/mixed request and response fields.
- `src/rpc.rs`: preserve lazy loading and map worker failures to stable status
  codes.

### Protocol and TypeScript

- `schema/opencode/memory/daemon/v1/daemon.proto`: describe one immutable
  multimodal profile and worker capabilities.
- `schema/opencode/memory/v1/memory.proto`: add content type, image ingest,
  image query, and filters.
- `opencode-memory/src/generated/`: regenerate bindings.
- `opencode-memory/src/daemon-client.ts`: send the profile identity and new
  multimodal requests.
- `opencode-memory/src/plugin.ts`: expose explicit image ingest and search
  options while keeping automatic visual recall opt-in.
- `opencode-memory/src/protocol.ts`: validate additive multimodal fields.
- native package build files: stage the selected worker runtime and licenses.

### Tests and benchmarks

- add a text/code/Vietnamese retrieval corpus larger than the current smoke
  benchmark;
- add image, screenshot, and mixed-input fixtures with redistribution-safe
  assets;
- add official-reference vector fixtures keyed by profile ID;
- add worker lifecycle and RSS probes;
- add migration, rollback, protocol, package, and offline-start tests.

## Final Recommendation

Using one Qwen3-VL embedding model is architecturally cleaner than using a
dedicated text encoder plus a visual encoder:

- one worker;
- one active dense space;
- one zvec collection generation;
- direct text-to-image and image-to-text retrieval;
- no dense cross-space rank fusion;
- simpler protocol and configuration.

`Qwen3-VL-Embedding-8B` is not currently the simpler deployment, however. Its
official artifact is substantially larger than the current model, and its
supported execution paths do not match the project's Rust-native, local,
cross-platform package. The model should drive a focused Phase 0 spike, not an
immediate implementation commitment.

The go decision requires both:

- a production-owned runtime with official-reference parity on every target
  platform;
- measured active-memory and cold-start behavior acceptable for the declared
  hardware tier.

If those gates pass, this single-model plan supersedes the generalized
multi-model architecture for v1. If they fail, use the 2B checkpoint with the
same single-space architecture or return to the separate model plan.

## Primary References

- [Qwen3-VL-Embedding-8B model card](https://huggingface.co/Qwen/Qwen3-VL-Embedding-8B)
- [Qwen3-VL-Embedding-8B Hub API](https://huggingface.co/api/models/Qwen/Qwen3-VL-Embedding-8B?blobs=true)
- [Qwen3-VL-Embedding-2B model card](https://huggingface.co/Qwen/Qwen3-VL-Embedding-2B)
- [Official Qwen3-VL-Embedding repository](https://github.com/QwenLM/Qwen3-VL-Embedding)
- [Official embedding implementation](https://github.com/QwenLM/Qwen3-VL-Embedding/blob/main/src/models/qwen3_vl_embedding.py)
- [Official Python dependencies](https://github.com/QwenLM/Qwen3-VL-Embedding/blob/main/pyproject.toml)
- [Technical report](https://arxiv.org/abs/2601.04720)
- [vLLM multimodal embedding documentation](https://docs.vllm.ai/en/latest/models/pooling_models/embed/)
- [llama.cpp Qwen3-VL embedding discussion](https://github.com/ggml-org/llama.cpp/discussions/19516)
- [llama.cpp multimodal embedding PR 21103](https://github.com/ggml-org/llama.cpp/pull/21103)
- [llama.cpp image-text embedding PR 18665](https://github.com/ggml-org/llama.cpp/pull/18665)
- [Community Qwen3-VL llama.cpp implementation](https://github.com/Tokimorphling/qwen3-vl-embedding)
- [vLLM versus Sentence Transformers parity report](https://github.com/QwenLM/Qwen3-VL-Embedding/issues/87)
- [vLLM Metal supported-model table](https://github.com/vllm-project/vllm-metal/blob/main/docs/supported_models.md)
