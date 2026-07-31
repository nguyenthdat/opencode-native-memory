# OpenCode Memory Plugin

`@nguyenthdat/opencode-memory` adds durable, project-scoped memory to OpenCode.
The TypeScript plugin talks to one user-scoped Rust daemon, stores private memory
in zvec, creates embeddings locally with a GGUF model through `llama-cpp-2`, and
can synchronize reviewed repository memory from Markdown.

This is local-first, not an absolute no-egress system. Automatic recall inserts
selected memories into the active OpenCode model context, so a remote model may
receive recalled content. Through the first-party graph tool, extraction sends
eligible source units to the active provider after OpenCode permission is
granted. Durable graph jobs may resume without asking for permission again. The
native protocol validates source eligibility but does not persist a permission
receipt. Storage, embeddings, indexing, lifecycle management, and graph
persistence remain local.

For implementation details, benchmarks, and release information, see the
[repository README](../README.md). The Protobuf contracts are documented in the
[schema README](../schema/README.md).

## Requirements

- Bun `>=1.3.0`
- OpenCode `>=1.18.4`
- Server plugin API `@opencode-ai/plugin >=1.18.4 <2`
- A supported native package installed as an optional dependency

The TUI companion additionally requires resolvable `@opentui/solid >=0.4.5` and
`solid-js 1.9.12`.

Supported prebuilt targets:

| Platform    | Architecture | Native package                                 |
| ----------- | ------------ | ---------------------------------------------- |
| macOS       | ARM64        | `@nguyenthdat/opencode-memory-darwin-arm64`    |
| Linux glibc | ARM64        | `@nguyenthdat/opencode-memory-linux-arm64-gnu` |
| Linux glibc | x64          | `@nguyenthdat/opencode-memory-linux-x64-gnu`   |

macOS x64, Linux musl, Windows, and other targets do not currently have a
prebuilt package.

## Installation

Add the server plugin to `opencode.json` or `opencode.jsonc`:

```json
{
  "plugin": ["@nguyenthdat/opencode-memory"]
}
```

Enable the optional health companion in `tui.json`:

```json
{
  "$schema": "https://opencode.ai/tui.json",
  "plugin": ["@nguyenthdat/opencode-memory/tui"]
}
```

Install optional dependencies normally. The plugin intentionally has no
postinstall downloader and does not install a global service. The matching
native package supplies the daemon executable and zvec shared library. The
configured GGUF embedding model may be downloaded from Hugging Face when a
project engine is first initialized.

## Runtime Model

Importing the package does not by itself start a native process. With the
default `warmup` and automatic document-index settings, plugin initialization
does issue native requests, so the daemon normally starts shortly after
OpenCode loads the plugin.

The runtime has these ownership boundaries:

```text
OpenCode server plugin
  -> shared daemon client and project lease
  -> length-delimited Protobuf over a private Unix socket
User-scoped Rust daemon
  -> canonical project registry
  -> one serialized ProjectActor per active store
  -> one MemoryEngine, model context, and writer.lock per project
Project store
  -> zvec collection, lifecycle state, document manifest, graph sidecar
```

One daemon can serve multiple OpenCode processes. A startup lock prevents
concurrent clients from launching duplicate daemons, and a lifetime lock keeps
different daemon generations from owning the same endpoint. Clients negotiate
the daemon protocol, domain schema generation, capabilities, and package
version before acquiring a project lease.

The daemon uses a heartbeat every 10 seconds and a 30-second session lease. By
default, unleased project actors are evicted after 5 minutes and the daemon exits
after 10 minutes without sessions or project activity. A project client cannot
request global daemon shutdown.

## Commands

The server plugin registers `/memory` when the project has not already defined
that command. With no arguments it inspects status and lists memory. It can also
guide search, lifecycle operations, diagnostics, promotion, and model-profile
preflight.

The TUI companion registers `/memory-health`, refreshes the native status, and
shows a `Memory` section with one of these states in the sidebar:

- `• Checking`
- `• Healthy`
- `• Degraded`
- `• Unavailable`

## Tools

The plugin exposes 29 OpenCode tools.

### Retrieval And Storage

| Tool              | Purpose                                                                      |
| ----------------- | ---------------------------------------------------------------------------- |
| `memory_search`   | Search with lexical, dense, or hybrid retrieval and optional graph fusion    |
| `memory_store`    | Store one distilled durable memory or evidence-backed personalization record |
| `memory_get`      | Fetch complete records by exact ID                                           |
| `memory_list`     | Filter and paginate lifecycle-indexed records                                |
| `memory_feedback` | Record `used`, `ignored`, or `error` feedback for recalled IDs               |
| `memory_export`   | Export visible records, lifecycle relations, and tombstones                  |
| `memory_import`   | Validate and import a portable snapshot; permission-gated                    |

### Documents And Sharing

| Tool                     | Purpose                                                      |
| ------------------------ | ------------------------------------------------------------ |
| `memory_ingest`          | Queue one project-relative PDF, Markdown, or HTML document   |
| `memory_ingest_status`   | Inspect the plugin-local background ingestion job            |
| `memory_index_documents` | Incrementally index supported, non-ignored project documents |
| `memory_promote`         | Write reviewed local memory to `.opencode/memory/<id>.md`    |

Document inputs must be regular, non-symlinked files contained by the project.
Supported extensions are `.pdf`, `.md`, `.markdown`, `.html`, and `.htm`.
Automatic indexing respects Git and ignore rules and excludes
`.opencode/memory/`, which is managed by shared-memory synchronization.

Document paths are limited to 200 characters, source files to 32 MiB, extracted
content to 600,000 characters, and extracted documents to 100 chunks. Automatic
discovery is limited to 1,000 documents. Shared synchronization accepts at most
200 Markdown files of at most 64 KiB each. Plugin-local ingestion status is
retained for 30 minutes and is lost when the plugin is disposed.

Ingestion jobs are serialized inside the plugin process. Their status records
do not survive plugin disposal, but memory already committed by a completed job
is durable.

### Lifecycle And Maintenance

| Tool              | Purpose                                                                                             |
| ----------------- | --------------------------------------------------------------------------------------------------- |
| `memory_update`   | Correct semantic content or lifecycle metadata                                                      |
| `memory_pin`      | Pin or unpin without re-embedding                                                                   |
| `memory_lock`     | Lock or unlock without re-embedding                                                                 |
| `memory_delete`   | Batch delete, creating tombstones by default                                                        |
| `memory_purge`    | Confirm and logically purge project records while preserving shared Markdown and the physical store |
| `memory_optimize` | Prune eligible state and optimize native indexes                                                    |
| `memory_doctor`   | Diagnose schema, index, retention, anchor, and cache health                                         |
| `memory_status`   | Return backend status plus plugin-level health issues                                               |

Repository-scoped records are canonical Markdown. They cannot be updated,
pinned, locked, or deleted through private lifecycle tools; edit their source
under `.opencode/memory/` instead.

### Model Profiles

| Tool                         | Purpose                                               |
| ---------------------------- | ----------------------------------------------------- |
| `memory_model_profiles`      | List stable, preview, and unsupported profiles        |
| `memory_model_switch`        | Start or dry-run a durable model-generation migration |
| `memory_model_switch_status` | Read durable model-switch progress                    |
| `memory_model_switch_cancel` | Request cooperative cancellation before cutover       |

The public model-switch tool asks permission for apply operations, creates a
durable switch journal, and returns immediately with a switch ID. The daemon
performs validation, artifact loading, target-generation preparation,
reindexing, verification, and cutover in bounded actor-owned steps. Status and
cooperative cancellation are public. `dry_run=true` performs preflight only.
`retain_previous=true` keeps the previous generation available for a later
explicit rollback request. A switch never changes environment variables or
restarts the daemon. Changing vector-affecting configuration for an existing
project is rejected instead of mixing incompatible vectors.

### Knowledge Graph

| Tool                              | Purpose                                                         |
| --------------------------------- | --------------------------------------------------------------- |
| `memory_graph_extract`            | Permission-gated extraction using the active OpenCode provider  |
| `memory_graph_extract_status`     | Inspect and resume a durable extraction job                     |
| `memory_graph_extract_cancel`     | Cooperatively cancel queued or running graph work               |
| `memory_graph_search`             | Search visible entities and relations through bounded traversal |
| `memory_reflect`                  | Build a bounded citation-ready reflection evidence packet       |
| `memory_graph_status`             | Inspect source-visible counts and the latest extraction         |
| `memory_graph_export`             | Export one bounded page of facts and provenance                 |
| `memory_graph_observation_action` | Review, edit, invalidate, or restore an observation             |

Graph extraction is explicit provider egress through the first-party tool. It
requests permission only when eligible source units exist, including for
`dry_run=true`; a dry run avoids creating a durable native job but still sends
eligible source units to the active provider. Durable jobs may resume after
plugin restart without another prompt. Repository-scoped sources, sources with
code anchors, fixed code-like source suffixes, secret-like content,
prompt-injection-shaped content, stale, expired, superseded, or otherwise
ineligible sources are blocked before dispatch. The daemon revalidates source
scope, visibility, hash, revision, evidence, and policy before committing graph
entities, relations, world/experience facts, and observations. Oxigraph keeps
an in-process RDF projection while Zvec supplies generation-safe semantic
vectors. Arbitrary native protocol callers are not covered by the first-party
permission receipt flow.

## Memory Semantics

Memory kinds:

```text
decision, preference, fact, pattern, gotcha, summary
```

`gotcha` is not a taxonomy. Canonical writes use `kind: "gotcha"` and infer
`fix_pattern`; canonical search/list filters use `kinds: ["gotcha"]`. The
plugin accepts `taxonomy: "gotcha"` as a compatibility alias and normalizes it
before sending the native request.

Memory scopes:

| Scope        | Behavior                                              |
| ------------ | ----------------------------------------------------- |
| `session`    | Shared by a primary session and its related subagents |
| `agent`      | Limited to the current OpenCode agent role            |
| `project`    | Durable and private across project sessions           |
| `repository` | Reviewed Markdown intended for Git sharing            |

Writable RPC scopes are `session`, `agent`, and `project`. The five dedicated
personalization taxonomies are `user_identity`, `user_behavior`,
`user_preference`, `user_goal`, and `user_relationship`. Manual and automatic
personalization capture requires a direct evidence quote from a user message;
the quote is used for validation and is not persisted as a separate secret
field.

Automatic recall performs a bounded hybrid search before model execution,
filters stale and superseded records, and inserts selected results as historical
context. Automatic capture evaluates at most three curated candidates after
session compaction and does not store the raw conversation or compaction
summary. Opt-in automatic retain may separately store a bounded, expiring
session-summary source for source-backed fact extraction.

Provider-backed automatic retain is a separate opt-in. When
`automaticRetain=true`, successful explicit document ingestion may enqueue a
bounded durable graph/fact extraction job after the normal provider permission
prompt, and session compaction may enqueue a bounded session-scoped source when
the active model is known. Completed tool outcomes can retain only the tool
name, outcome class, bounded title, and duration; raw input/output/error logs
are excluded. It remains disabled by default and does not relax repository/code
egress guards.

## Configuration

`createMemoryPlugin()` accepts these options. Explicit options take precedence
over their environment-variable equivalents.

| Option                    |               Default | Environment variable                         |
| ------------------------- | --------------------: | -------------------------------------------- |
| `root`                    | required package root | none                                         |
| `projectRoot`             |     OpenCode worktree | none                                         |
| `warmup`                  |                `true` | `OPENCODE_MEMORY_WARMUP`                     |
| `automaticRecall`         |                `true` | `OPENCODE_MEMORY_AUTO_RECALL`                |
| `automaticCapture`        |                `true` | `OPENCODE_MEMORY_AUTO_CAPTURE`               |
| `automaticRetain`         |               `false` | `OPENCODE_MEMORY_AUTO_RETAIN`                |
| `automaticRetainSources`  |           all classes | `OPENCODE_MEMORY_AUTO_RETAIN_SOURCES`        |
| `automaticDocumentIndex`  |                `true` | `OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS`       |
| `documentIndexDebounceMs` |                 `750` | `OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS` |
| `automaticOptimize`       |                `true` | `OPENCODE_MEMORY_AUTO_OPTIMIZE`              |
| `optimizeDebounceMs`      |                `5000` | `OPENCODE_MEMORY_OPTIMIZE_DEBOUNCE_MS`       |
| `sharedSync`              |                `true` | `OPENCODE_MEMORY_SHARED_SYNC`                |
| `feedbackTracking`        |                `true` | `OPENCODE_MEMORY_FEEDBACK_TRACKING`          |
| `minScore`                |                `0.42` | `OPENCODE_MEMORY_MIN_SCORE`                  |

Boolean environment variables accept `1`, `true`, `yes`, or `on` and `0`,
`false`, `no`, or `off`, case-insensitively. Debounce values must be between 50
and 60,000 milliseconds. `minScore` must be between 0 and 1.

### Embedding Configuration

| Variable                                     | Purpose                                           |
| -------------------------------------------- | ------------------------------------------------- |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_PATH`       | Use an existing local GGUF model                  |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_REPO`       | Override the Hugging Face repository              |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_FILE`       | Override the GGUF filename                        |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_REVISION`   | Select an immutable 40-character commit revision  |
| `OPENCODE_MEMORY_EMBEDDING_POOLING`          | Pooling mode such as `last`, `mean`, or `cls`     |
| `OPENCODE_MEMORY_EMBEDDING_ATTENTION`        | `causal`, `non_causal`, or model/default behavior |
| `OPENCODE_MEMORY_EMBEDDING_QUERY_TEMPLATE`   | Query template containing `{text}`                |
| `OPENCODE_MEMORY_EMBEDDING_PASSAGE_TEMPLATE` | Passage template containing `{text}`              |
| `OPENCODE_MEMORY_EMBEDDING_ADD_BOS`          | Add a beginning-of-sequence token                 |
| `OPENCODE_MEMORY_EMBEDDING_APPEND_EOS`       | Append an end-of-sequence token                   |
| `OPENCODE_MEMORY_EMBEDDING_NORMALIZE`        | L2-normalize embeddings                           |
| `OPENCODE_MEMORY_EMBEDDING_DIMENSION`        | Native or lower MRL output dimension              |
| `OPENCODE_MEMORY_EMBEDDING_CONTEXT_SIZE`     | llama.cpp context size                            |
| `OPENCODE_MEMORY_EMBEDDING_THREADS`          | Requested inference thread cap                    |
| `OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS`       | Number of layers to offload                       |

The default stable profile is `qwen3-text-4b-q4`, backed by
`Qwen/Qwen3-Embedding-4B-GGUF` at a pinned revision. A custom model must be a
GGUF embedding model compatible with the bundled llama.cpp revision. A
safetensors-only repository cannot be loaded directly.

### Storage And Daemon Configuration

| Variable                                       | Default or behavior                                                 |
| ---------------------------------------------- | ------------------------------------------------------------------- |
| `OPENCODE_MEMORY_DATA_DIR`                     | Replaces the complete private data root                             |
| `OPENCODE_MEMORY_MODEL_CACHE`                  | Replaces the complete model-cache path                              |
| `OPENCODE_MEMORY_REQUEST_TIMEOUT_MS`           | 5 minutes; positive finite values clamp to 1 second through 2 hours |
| `OPENCODE_NATIVE_MEMORY_BIN`                   | Exclusive development binary override                               |
| `OPENCODE_MEMORY_PROJECT_IDLE_SECONDS`         | `300`                                                               |
| `OPENCODE_MEMORY_DAEMON_IDLE_SECONDS`          | `600`                                                               |
| `OPENCODE_MEMORY_MAINTENANCE_INTERVAL_SECONDS` | `300`                                                               |

Use the `projectRoot` factory option to override the project root in plugin
integrations. `OPENCODE_MEMORY_PROJECT_ROOT` applies to standalone native modes,
not normal daemon acquisition by this plugin.

### Content Scanner Configuration

| Variable                                        | Default or behavior                                     |
| ----------------------------------------------- | ------------------------------------------------------- |
| `OPENCODE_MEMORY_DISABLE_SECRET_SCANNER`        | `false`; disables all secret checks for this project    |
| `OPENCODE_MEMORY_DISABLE_PROMPT_INJECTION_SCAN` | `false`; disables all injection checks for this project |
| `OPENCODE_MEMORY_GUARDRAIL_ONNX_MODEL`          | Optional local prompt-injection ONNX model              |
| `OPENCODE_MEMORY_GUARDRAIL_ONNX_TOKENIZER`      | Matching local tokenizer JSON                           |
| `OPENCODE_MEMORY_GUARDRAIL_ONNX_THRESHOLD`      | Classification threshold; default `0.85`                |

Disable flags accept `1`, `true`, `yes`, or `on`, case-insensitively. They are
resolved by each plugin process and sent as project-acquisition policy, rather
than inherited globally by the shared daemon. Disabling a scanner removes a
safety boundary; only do so for inputs and exports you independently trust and
review.

## Storage

The default data root is:

```text
$XDG_DATA_HOME/opencode/memory
```

or, when `XDG_DATA_HOME` is not set:

```text
$HOME/.local/share/opencode/memory
```

Each canonical project uses a SHA-256 path identity:

```text
<data-root>/projects/<project-id>/
  active-embedding.json
  model-switch.json
  document-index.json
  knowledge-graph.json
  knowledge-graph.pending.json
  manifest.json
  state.json
  writer.lock
  zvec/                         # legacy collection, when applicable
  embedding-generations/
    <generation-id>/
      generation.json
      manifest.json
      zvec/
```

Repository-shared memory is separate and lives in:

```text
.opencode/memory/**/*.md
```

Shared Markdown is treated as untrusted input. Paths are contained under the
worktree, symlinks and unknown frontmatter fields are rejected, and imported
repository records cannot set private pin or lock state.

## Security And Privacy

- Runtime directories use mode `0700`; sockets and lock files use mode `0600`.
- The client validates runtime-directory and socket-path ownership, file type, symlink status, and mode; the daemon validates connecting clients' Unix peer UID.
- Same-UID processes remain within the trust boundary, and project-store files do not receive identical validation on every open.
- Secret checks apply to stored memory fields. Instruction-shaped-content rejection applies to automatic compaction, ingested documents, shared Markdown, and graph remote-eligibility checks; manual memory is not rejected solely for containing instruction-shaped text.
- Code anchors must resolve to regular project files and are stored with content hashes.
- Native graph state stores provider and model identifiers but does not serialize credential values. The daemon inherits the plugin process environment, so environment-supplied credentials may be present in daemon process memory.
- Private stores rely on filesystem permissions and are not encrypted at rest.
- Secret detection is heuristic and is not a comprehensive DLP guarantee.
- Initial model setup may contact Hugging Face.
- Recalled memory may enter a remote model context when the active OpenCode provider is remote.
- Through the first-party tool, graph extraction sends only explicitly approved, policy-eligible source units to the selected provider. Resumed durable jobs do not prompt again.

## Programmatic API

The package exposes three public subpaths:

| Import                                | API                                          |
| ------------------------------------- | -------------------------------------------- |
| `@nguyenthdat/opencode-memory`        | Default server plugin plus named SDK exports |
| `@nguyenthdat/opencode-memory/server` | `{ id, server }` plugin module               |
| `@nguyenthdat/opencode-memory/tui`    | `{ id, tui }` module and TUI helpers         |

The root entrypoint exports the plugin factory and options, contract constants
and types, daemon client and pooling types, protocol encoders/decoders, graph
extractor and validation helpers, model preflight helpers, maintenance and
outcome-reconciliation helpers, session context, instruction registration,
policy helpers, and shared-Markdown helpers. Only the three export-map subpaths
above are public; internal `dist/*` modules are not stable package entrypoints.

Example custom plugin factory. `root` must resolve to the installed package root,
not the consumer plugin directory:

```ts
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createMemoryPlugin } from "@nguyenthdat/opencode-memory";

const packageRoot = resolve(
  dirname(fileURLToPath(import.meta.resolve("@nguyenthdat/opencode-memory"))),
  "..",
);

export default createMemoryPlugin({
  root: packageRoot,
  automaticRecall: true,
  automaticCapture: true,
  minScore: 0.5,
});
```

`root` is the installed package root containing `package.json` and
`rules/native-memory.md`; it is not the project worktree. Most OpenCode users
should use the package's default export instead of constructing the plugin.

## Development

Repository prerequisites are Bun 1.3+, Rust 1.97+, `protoc`, Buf, CMake, and a
working C/C++ toolchain. Apple Silicon Metal builds also require the system Metal
frameworks.

```sh
bun install
bun run generate:protocol:check
bun run lint:proto
bun run format:check
bun run typecheck
bun run test:ts
cargo test --locked --lib
bun run build:native
bun run test:protocol
bun run build
bun run pack:check
```

Native acceleration features are opt-in: `metal`, `cuda`, `cuda-no-vmm`,
`vulkan`, `openmp`, and `static-openmp`.

## Current Limitations

- Prebuilt native packages support only Apple Silicon macOS and glibc Linux ARM64/x64.
- Model switching is durable and permission-gated; apply, status, cancellation, and retained-generation rollback are distinct operations.
- Memory lifecycle state schema v4 is new-only; older state files are rejected instead of migrated.
- The daemon transport is Unix-domain-socket Protobuf, not gRPC.
- Stores are not encrypted at rest.
- Ingestion job status is local to the plugin process; durable model-switch and graph-extraction journals survive daemon/plugin restart.

## License

MIT. Third-party notices are included in `THIRD_PARTY_NOTICES.md` and `notices/`.
