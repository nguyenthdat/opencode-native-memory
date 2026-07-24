# OpenCode Native Memory Demo

This project loads the local `0.3.1` source build from `../../dist/index.js` and keeps its memory store under `.memory-data/`, isolated from the parent repository store.

## Start

```sh
cd tests/opencode-memory-demo
bun run prepare
bun run start
```

Quit and restart OpenCode after changing `opencode.jsonc`, `memory-plugin.ts`, or the plugin source because OpenCode loads configuration and plugins only at startup.

The first run may download the default `Qwen3-Embedding-4B-Q4_K_M.gguf` model under `~/.local/share/opencode/memory/models/<model-revision>/`. To reuse an existing cache:

```sh
export OPENCODE_MEMORY_MODEL_CACHE=/absolute/path/to/model-cache
bun run start
```

Inside OpenCode, run:

```text
/memory-smoke
```

The plugin automatically registers its packaged `rules/flow.md`; the demo config intentionally lists only `AGENTS.md` to exercise that integration.

Useful manual checks:

```text
Call memory_status and show protocol, schema, model, dimensions, and capabilities.
Search memory for the demo server entry point.
Try deleting the repository-scoped architecture record and confirm the backend rejects it.
Export a snapshot, purge with explicit confirmation, import the snapshot JSON, then search again.
```

## Reset

Stop OpenCode, remove `.memory-data/`, and restart. The shared Markdown fixture under `.opencode/memory/` will be indexed again.
