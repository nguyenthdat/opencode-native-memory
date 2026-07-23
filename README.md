# OpenCode Native Memory

Local-first persistent memory for OpenCode. The plugin runs a native Rust sidecar, stores project-scoped memories in zvec, and embeds text locally with llama.cpp. No memory content or embedding request is sent to a hosted inference service.

## Highlights

- Hybrid dense, lexical, metadata, and feedback-aware retrieval
- Local GGUF embeddings through `utilityai/llama-cpp-rs`
- Default pinned `Qwen3-Embedding-4B` model from Hugging Face
- Session-family, agent, project, and repository scopes
- Durable taxonomy, confidence, supersession, conflict, pin, lock, expiry, and tombstone metadata
- Deterministic capture gate with quarantine, skip, duplicate, supersession, and conflict outcomes
- Crash-recoverable batch upsert journal and portable export/import snapshots
- Markdown-backed shared repository memory under `.opencode/memory/`
- Length-delimited Protobuf protocol between TypeScript and Rust
- Native sidecar packages for macOS, Linux, and Windows

## Install

Add the plugin to `opencode.json` or `opencode.jsonc`:

```json
{
  "plugin": ["@nguyenthdat/opencode-memory@0.3.0"]
}
```

npm installs one matching optional native package for the current platform. Reinstall without `--omit=optional`; the plugin intentionally has no postinstall script or runtime binary download.

Supported packages:

| OS          | Architecture | Native package                                 |
| ----------- | ------------ | ---------------------------------------------- |
| macOS       | ARM64        | `@nguyenthdat/opencode-memory-darwin-arm64`    |
| macOS       | x64          | `@nguyenthdat/opencode-memory-darwin-x64`      |
| Linux glibc | ARM64        | `@nguyenthdat/opencode-memory-linux-arm64-gnu` |
| Linux glibc | x64          | `@nguyenthdat/opencode-memory-linux-x64-gnu`   |
| Windows     | x64 MSVC     | `@nguyenthdat/opencode-memory-win32-x64-msvc`  |

The first memory operation downloads the default GGUF model into the local model cache. Override `OPENCODE_MEMORY_EMBEDDING_MODEL_PATH` to use an existing local model and avoid a network download.

Add `rules/flow.md` to the project instructions if memory tool usage should be explicit for every agent.

## Memory Tools

| Tool              | Purpose                                              |
| ----------------- | ---------------------------------------------------- |
| `memory_search`   | Retrieve relevant memories within a context budget   |
| `memory_store`    | Store a verified durable memory                      |
| `memory_get`      | Fetch complete records by ID                         |
| `memory_list`     | Review/filter lifecycle-indexed memories             |
| `memory_update`   | Correct semantic content or lifecycle metadata       |
| `memory_pin`      | Pin or unpin without re-embedding                    |
| `memory_lock`     | Lock or unlock without re-embedding                  |
| `memory_delete`   | Delete records, with tombstones by default           |
| `memory_promote`  | Promote reviewed local memory to repository Markdown |
| `memory_export`   | Export records, lifecycle relations, and tombstones  |
| `memory_import`   | Validate and restore a portable JSON snapshot        |
| `memory_feedback` | Record whether recalled memories were useful         |
| `memory_optimize` | Prune expired records and optimize indexes           |
| `memory_status`   | Inspect backend, model, and schema status            |
| `memory_doctor`   | Run shallow or deep integrity checks                 |
| `memory_purge`    | Confirm and delete the complete project store        |

The 15 stable taxonomy values are `task_attempt`, `tool_call`, `session_summary`, `architecture_fact`, `codebase_fact`, `user_fact`, `fix_pattern`, `code_template`, `tool_heuristic`, `code_style`, `library_pref`, `workflow_pref`, `decision`, `team_convention`, and `project_standard`.

## Embedding Models

The default is:

- Repository: `Qwen/Qwen3-Embedding-4B-GGUF`
- File: `Qwen3-Embedding-4B-Q4_K_M.gguf`
- Revision: `f4602530db1d980e16da9d7d3a70294cf5c190be`
- Native dimension: 2560
- Pooling: last token
- Normalization: L2

"Any Hugging Face model" means any **GGUF embedding model compatible with the bundled llama.cpp revision**. Safetensors-only repositories are not loaded directly. Model templates and pooling must match the chosen model.

Changing model identity or embedding dimension requires rebuilding the project's vector index. The sidecar rejects a mismatched existing collection instead of silently mixing incompatible vectors.

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
| `OPENCODE_MEMORY_EMBEDDING_THREADS`          | Available parallelism                                                        |
| `OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS`       | All layers when GPU offload is supported, otherwise `0`                      |
| `OPENCODE_MEMORY_PROJECT_ROOT`               | Override project discovery root                                              |
| `OPENCODE_MEMORY_DATA_DIR`                   | Override project store base directory                                        |
| `OPENCODE_MEMORY_MODEL_CACHE`                | Override local Hugging Face model cache                                      |
| `OPENCODE_NATIVE_MEMORY_BIN`                 | Development/debug sidecar override                                           |
| `OPENCODE_MEMORY_WARMUP`                     | Enable model/shared-memory warmup; default `true`                            |
| `OPENCODE_MEMORY_AUTO_RECALL`                | Enable automatic contextual recall; default `true`                           |
| `OPENCODE_MEMORY_AUTO_CAPTURE`               | Evaluate compaction candidates through the capture gate; default `true`      |
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

Private state uses the platform data directory under `opencode/memory/<project-id>/`. Models use the platform cache directory under `opencode/memory/models/`.

Repository memory is canonical Markdown in:

```text
.opencode/memory/
  architecture.md
  conventions.md
  gotchas.md
  decisions/
```

Shared Markdown is treated as untrusted data: paths are contained under `.opencode/memory`, YAML is parsed with a strict schema, instruction-shaped content and likely secrets are rejected, and imported records cannot be pinned or locked through RPC.

## Architecture

```text
OpenCode plugin (TypeScript)
  -> length-delimited Protobuf over stdin/stdout
Rust sidecar
  -> lifecycle/taxonomy policy
  -> llama.cpp GGUF embedder
  -> zvec vector + FTS collection
  -> atomic JSON lifecycle state
```

The Protobuf schema is `schema/opencode/memory/v1/memory.proto`. Rust bindings are generated at Cargo build time with `prost-build`; TypeScript bindings are committed under `opencode-memory/src/generated/opencode/memory/v1/` and reproduced with `bun run generate:protocol`.

Lifecycle state schema v4 is intentionally new-only. Older state schemas are rejected instead of migrated; move or purge an older project store before using this build. Upserts are journaled before zvec mutation and replayed as an order-independent batch when the engine opens.

## Development

Requirements: Bun 1.3+, Rust 1.97+, `protoc`, and Buf.

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

An isolated project fixture is available at `tests/opencode-memory-demo`. Its config lives at `.opencode/opencode.jsonc`; it loads the local `dist/` plugin, uses the local release sidecar, keeps data under its own `.memory-data/`, and includes a `/memory-smoke` command.

```sh
cd tests/opencode-memory-demo
bun run prepare
bun run start
```

Build the local sidecar:

```sh
bun run build:native:release
```

GPU features are opt-in Cargo features: `metal`, `cuda`, `cuda-no-vmm`, `vulkan`, `openmp`, and `static-openmp`.

## Releases

Tags matching `vX.Y.Z` build and package the five native targets, publish native packages first, publish the umbrella plugin with npm provenance, and create a GitHub release containing all tarballs and a checksum for the umbrella package. `package.json`, `Cargo.toml`, and every native package must carry the same version.

## License

MIT. Bundled dependency notices are in `THIRD_PARTY_NOTICES.md` and `notices/`.
