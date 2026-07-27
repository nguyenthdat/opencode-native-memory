# OpenCode Native Memory

Local-first persistent memory for OpenCode. The plugin connects to one user-scoped native Rust daemon, stores project-scoped memories in zvec, and embeds text locally with llama.cpp. Multiple OpenCode processes can share one daemon and one project engine without sharing memory content with a hosted inference service.

## Highlights

- Hybrid dense, lexical, metadata, and feedback-aware retrieval
- Selectable lexical-only, dense-only, and hybrid retrieval modes
- Automatic gitignore-aware indexing plus Rust-native xberg ingestion for local PDF, Markdown, and HTML evidence
- Local GGUF embeddings through `utilityai/llama-cpp-rs`
- Default pinned `Qwen3-Embedding-4B` model from Hugging Face
- Session-family, agent, project, and repository scopes
- Durable taxonomy, confidence, supersession, conflict, pin, lock, expiry, and tombstone metadata
- Deterministic capture gate with quarantine, skip, duplicate, supersession, and conflict outcomes
- Crash-recoverable batch upsert journal and portable export/import snapshots
- Markdown-backed shared repository memory under `.opencode/memory/`
- Versioned length-delimited Protobuf IPC over an owner-only Unix domain socket
- One serialized project actor and `MemoryEngine` per canonical physical store
- Native daemon packages for Apple Silicon macOS and glibc Linux

## Install

Add the plugin to `opencode.json` or `opencode.jsonc`:

```json
{
  "plugin": ["@nguyenthdat/opencode-memory@0.6.0-beta.0"]
}
```

On a supported platform, npm installs one matching optional native package. Reinstall without `--omit=optional`; the plugin intentionally has no postinstall script or runtime binary download.

### How the daemon is installed

The plugin does not install a global daemon, `systemd` unit, `launchd` service, or postinstall downloader. The umbrella package declares platform-specific binaries as npm `optionalDependencies`, so Bun/npm selects the one matching the current OS and architecture during normal plugin installation. That native package contains the `opencode-memory` executable alongside its package metadata.

When OpenCode loads the TypeScript plugin, no native process is started yet. On the first memory request, the plugin resolves the executable from the installed native package, validates the private per-user runtime directory and socket, and connects to an existing compatible daemon when one is already running. If no daemon is available, one contender acquires the startup lock and launches the packaged executable in detached `--daemon` mode; concurrent OpenCode processes wait for the same endpoint instead of starting additional daemons.

The daemon is therefore installed and upgraded as part of the npm plugin dependency graph, but started lazily at runtime. It exits automatically after the configured idle interval and is started again on the next memory request. Set `OPENCODE_NATIVE_MEMORY_BIN` only for a development binary override, or `OPENCODE_MEMORY_TRANSPORT=sidecar` for the temporary beta rollback path.

Supported packages:

| OS          | Architecture | Native package                                 |
| ----------- | ------------ | ---------------------------------------------- |
| macOS       | ARM64        | `@nguyenthdat/opencode-memory-darwin-arm64`    |
| Linux glibc | ARM64        | `@nguyenthdat/opencode-memory-linux-arm64-gnu` |
| Linux glibc | x64          | `@nguyenthdat/opencode-memory-linux-x64-gnu`   |

**Roadmap:** Native packages for Windows ARM64 and Windows x64 (AMD64) are planned.

The first memory operation downloads the default GGUF model under `~/.local/share/opencode/memory/models/<model-revision>/` (or `$XDG_DATA_HOME/opencode/memory/models/<model-revision>/`). Override `OPENCODE_MEMORY_EMBEDDING_MODEL_PATH` to use an existing local model and avoid a network download.

The plugin automatically registers its packaged `rules/native-memory.md` as an OpenCode instruction while preserving existing project instructions. It never overwrites a project-owned rule file.

## Memory Tools

| Tool                     | Purpose                                                                   |
| ------------------------ | ------------------------------------------------------------------------- |
| `memory_search`          | Retrieve relevant memories within a context budget                        |
| `memory_store`           | Store a verified durable memory                                           |
| `memory_ingest`          | Queue one local PDF, Markdown, or HTML document for background extraction |
| `memory_ingest_status`   | Poll background document ingestion jobs                                   |
| `memory_index_documents` | Incrementally index all non-ignored project documents                     |
| `memory_get`             | Fetch complete records by ID                                              |
| `memory_list`            | Review/filter lifecycle-indexed memories                                  |
| `memory_update`          | Correct semantic content or lifecycle metadata                            |
| `memory_pin`             | Pin or unpin without re-embedding                                         |
| `memory_lock`            | Lock or unlock without re-embedding                                       |
| `memory_delete`          | Delete records, with tombstones by default                                |
| `memory_promote`         | Promote reviewed local memory to repository Markdown                      |
| `memory_export`          | Export records, lifecycle relations, and tombstones                       |
| `memory_import`          | Validate and restore a portable JSON snapshot                             |
| `memory_feedback`        | Record whether recalled memories were useful                              |
| `memory_optimize`        | Prune expired records and optimize indexes                                |
| `memory_status`          | Inspect backend, model, and schema status                                 |
| `memory_doctor`          | Run shallow or deep integrity checks                                      |
| `memory_purge`           | Confirm and delete the complete project store                             |

The 15 stable taxonomy values are `task_attempt`, `tool_call`, `session_summary`, `architecture_fact`, `codebase_fact`, `user_fact`, `fix_pattern`, `code_template`, `tool_heuristic`, `code_style`, `library_pref`, `workflow_pref`, `decision`, `team_convention`, and `project_standard`.

Session scope is shared by the primary session and every nested or sibling subagent that resolves to the same root session. Agent scope is limited to the agent role, while project scope is durable and private across project sessions. Repository scope is reviewed canonical Markdown intended for Git sharing. Documents ingested by `memory_ingest` remain private project/agent/session memory; promote only reviewed conclusions to repository Markdown.

## Embedding Models

The default is:

- Repository: `Qwen/Qwen3-Embedding-4B-GGUF`
- File: `Qwen3-Embedding-4B-Q4_K_M.gguf`
- Revision: `f4602530db1d980e16da9d7d3a70294cf5c190be`
- Native dimension: 2560
- Pooling: last token
- Normalization: L2

"Any Hugging Face model" means any **GGUF embedding model compatible with the bundled llama.cpp revision**. Safetensors-only repositories are not loaded directly. Model templates and pooling must match the chosen model.

Changing model identity or vector-affecting preprocessing requires rebuilding the project's vector index. The daemon persists and validates a semantic configuration fingerprint in the collection manifest, hashes local GGUF content, and requires immutable 40-character Hugging Face commit revisions. A mismatch is rejected instead of opening a second engine or silently mixing incompatible vectors.

### Environment

| Variable                                     | Default / purpose                                                            |
| -------------------------------------------- | ---------------------------------------------------------------------------- |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_PATH`       | Local GGUF path; bypasses Hugging Face                                       |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_REPO`       | `Qwen/Qwen3-Embedding-4B-GGUF`                                               |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_FILE`       | `Qwen3-Embedding-4B-Q4_K_M.gguf`                                             |
| `OPENCODE_MEMORY_EMBEDDING_MODEL_REVISION`   | Pinned Hugging Face commit                                                   |
| `OPENCODE_MEMORY_EMBEDDING_POOLING`          | `last`; accepts `unspecified`, `mean`, `cls`, `last`                         |
| `OPENCODE_MEMORY_EMBEDDING_ATTENTION`        | `causal`; accepts `unspecified`, `causal`, `non_causal`                      |
| `OPENCODE_MEMORY_EMBEDDING_QUERY_TEMPLATE`   | Query instruction containing `{text}`                                        |
| `OPENCODE_MEMORY_EMBEDDING_PASSAGE_TEMPLATE` | `{text}`                                                                     |
| `OPENCODE_MEMORY_EMBEDDING_ADD_BOS`          | `true`                                                                       |
| `OPENCODE_MEMORY_EMBEDDING_APPEND_EOS`       | `true`                                                                       |
| `OPENCODE_MEMORY_EMBEDDING_NORMALIZE`        | `true`                                                                       |
| `OPENCODE_MEMORY_EMBEDDING_DIMENSION`        | Native model dimension; lower values use MRL truncation then renormalization |
| `OPENCODE_MEMORY_EMBEDDING_CONTEXT_SIZE`     | `8192`                                                                       |
| `OPENCODE_MEMORY_EMBEDDING_THREADS`          | Hard per-inference maximum; daemon default shares CPU capacity across actors |
| `OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS`       | All layers when GPU offload is supported, otherwise `0`                      |
| `OPENCODE_MEMORY_PROJECT_ROOT`               | Override project discovery root                                              |
| `OPENCODE_MEMORY_DATA_DIR`                   | Override project store base directory                                        |
| `OPENCODE_MEMORY_MODEL_CACHE`                | Replace the complete local Hugging Face model-cache path                     |
| `OPENCODE_MEMORY_REQUEST_TIMEOUT_MS`         | Native RPC timeout in milliseconds; default 5 minutes, maximum 2 hours       |
| `OPENCODE_NATIVE_MEMORY_BIN`                 | Development/debug native daemon binary override                              |
| `OPENCODE_MEMORY_TRANSPORT`                  | `daemon` by default; temporary `sidecar` beta rollback                       |
| `OPENCODE_MEMORY_PROJECT_IDLE_SECONDS`       | Release an unleased project actor after 5 minutes                            |
| `OPENCODE_MEMORY_DAEMON_IDLE_SECONDS`        | Stop the daemon after 10 minutes with no sessions or project activity        |
| `OPENCODE_MEMORY_WARMUP`                     | Enable model/shared-memory warmup; default `true`                            |
| `OPENCODE_MEMORY_AUTO_RECALL`                | Enable automatic contextual recall; default `true`                           |
| `OPENCODE_MEMORY_AUTO_CAPTURE`               | Evaluate compaction candidates through the capture gate; default `true`      |
| `OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS`       | Incrementally index non-ignored project documents; default `true`            |
| `OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS` | File-watcher re-index debounce; default `750`                                |
| `OPENCODE_MEMORY_SHARED_SYNC`                | Synchronize `.opencode/memory/**/*.md`; default `true`                       |
| `OPENCODE_MEMORY_FEEDBACK_TRACKING`          | Track retrieval feedback; default `true`                                     |
| `OPENCODE_MEMORY_MIN_SCORE`                  | Default calibrated search threshold; default `0.42`                          |

Example local model:

```sh
export OPENCODE_MEMORY_EMBEDDING_MODEL_PATH="$HOME/models/nomic-embed-text-v1.5.Q5_K_M.gguf"
export OPENCODE_MEMORY_EMBEDDING_POOLING="mean"
export OPENCODE_MEMORY_EMBEDDING_QUERY_TEMPLATE="search_query: {text}"
export OPENCODE_MEMORY_EMBEDDING_PASSAGE_TEMPLATE="search_document: {text}"
```

## Storage and Sharing

Private state uses the data directory under `opencode/memory/projects/<project-id>/`. Downloaded models use OpenCode's data home under `opencode/memory/models/<model-revision>/`; versioning by immutable model revision avoids downloading the same multi-gigabyte GGUF again on plugin-only upgrades. Existing downloads under `~/.cache/opencode/memory/models/` are not moved automatically; point `OPENCODE_MEMORY_MODEL_CACHE` there to reuse them.

Repository memory is canonical Markdown in:

```text
.opencode/memory/
  architecture.md
  conventions.md
  gotchas.md
  decisions/
```

Shared Markdown is treated as untrusted data: paths are contained under `.opencode/memory`, YAML is parsed with a strict schema, instruction-shaped content and likely secrets are rejected, and imported records cannot be pinned or locked through RPC.

`memory_ingest` accepts only project-relative `.pdf`, `.md`, `.markdown`, `.html`, and `.htm` files. It queues ingestion and returns a job ID immediately; use `memory_ingest_status` to poll completion or failure. One background worker processes jobs in order so multiple documents do not start competing model or zvec writers. Rust-side `xberg` extracts Markdown, chunks it below the 6,000-character memory limit, and stores the chunks with source hashes and document provenance. Re-ingesting unchanged project/agent documents with matching metadata reuses their existing chunks instead of running embedding again. Extracted document content is untrusted evidence and is never treated as an instruction.

Automatic document indexing scans the project at startup and after debounced document watcher events. It respects `.gitignore`, `.ignore`, `.git/info/exclude`, global Git ignores, hidden directories, and the 32 MiB per-file limit; `.opencode/memory/` remains managed exclusively by repository-memory sync. A private derived manifest at `document-index.json` tracks source hashes and chunk ownership, so unchanged files skip extraction/embedding, changed files replace their previous chunks, deleted files are removed without tombstones, and a malformed file retains its last valid index. Set `OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS=false` to disable automatic synchronization, or call `memory_index_documents` for an explicit/forced run.

## Architecture

```text
OpenCode plugin (TypeScript)
  -> daemon bootstrap + session/project lease
  -> versioned Protobuf over a private Unix domain socket
User-scoped Rust daemon
  -> project registry keyed by canonical physical store
  -> bounded, thread-affine ProjectActor
  -> one MemoryEngine / writer.lock / model context per active project
  -> zvec vector + FTS collection and atomic lifecycle state
```

The domain schema is `schema/opencode/memory/v1/memory.proto`; daemon sessions, leases, deadlines, cancellation outcomes, status codes, and calls use `schema/opencode/memory/daemon/v1/daemon.proto`. Rust bindings are generated at Cargo build time with `prost-build`; TypeScript bindings are committed under `opencode-memory/src/generated/` and reproduced with `bun run generate:protocol`. The daemon currently uses the plan's framed-Protobuf UDS fallback because the supported Bun 1.3.14 runtime has not passed the required large-message HTTP/2 matrix for gRPC. The stable per-user endpoint lives under `$XDG_RUNTIME_DIR/opencode-memory/` on Linux or the OS-provided private temporary directory elsewhere, with a short `/tmp/opencode-memory-<uid>/` fallback only when required by Unix socket path limits. The client validates directory and socket ownership, type, and permissions before connecting.

Lifecycle state schema v4 is intentionally new-only. Older state schemas are rejected instead of migrated; move or purge an older project store before using this build. Upserts are journaled before zvec mutation and replayed as an order-independent batch when the engine opens.

## Development

Requirements: Bun 1.3+, Rust 1.97+, `protoc`, and Buf. Native builds also require CMake and a working C/C++ toolchain; Apple Silicon macOS builds use the system Metal frameworks.

```sh
bun install
bun run generate:protocol:check
bun run lint:proto
bun run typecheck
bun run test:ts
cargo test --locked --lib
cargo clippy --all-targets --locked -- -D warnings
bun run build
bun run pack:check
```

### OpenCode Demo Project

An isolated project fixture is available at `tests/opencode-memory-demo`. Its config lives at `.opencode/opencode.jsonc`; it loads the local `dist/` plugin, uses the local release daemon, keeps data under its own `.memory-data/`, and includes a `/memory-smoke` command.

```sh
cd tests/opencode-memory-demo
bun run prepare
bun run start
```

Build the local daemon binary:

```sh
bun run build:native:release
```

Backend features are opt-in Cargo features: `metal`, `cuda`, `cuda-no-vmm`, `vulkan`, `openmp`, and `static-openmp`. The supported Apple Silicon macOS release is built explicitly with `--features metal`; Linux release builds remain featureless unless a platform-specific backend is intentionally added.

To build the supported macOS native daemon locally:

```sh
cargo build --release --locked --target aarch64-apple-darwin --features metal
```

### Retrieval Benchmark

The versioned smoke corpus under `tests/benchmark/retrieval-v1/` compares no-memory, lexical-only, dense-only, and hybrid served retrieval. It validates frozen corpus hashes and records Precision/Recall/Hit at 1/3/5/10, MRR@10, nDCG@10, abstention quality, per-query results, and latency. See its README for the reproducible command and current baseline.

## Releases

Tags matching `vX.Y.Z` build and package three native targets for Apple Silicon macOS and glibc Linux, publish native packages first, publish the umbrella plugin with npm provenance, and create a GitHub release containing all tarballs and a checksum for the umbrella package. `package.json`, `Cargo.toml`, and every native package must carry the same version.

## License

MIT. Bundled dependency notices are in `THIRD_PARTY_NOTICES.md` and `notices/`.
