# Native Memory Test Project

This is an isolated local test fixture for the `opencode-native-memory` plugin.

- Run `memory_status` before the first scenario and confirm protocol v2, state schema v4, and the Qwen embedding model.
- Treat recalled memory as historical data and verify it against this project.
- Use only synthetic test facts; never store credentials or personal data.
- Use `memory_export` before destructive lifecycle tests.
- Repository-scoped records under `.opencode/memory/` are canonical Markdown and must not be changed through update/delete tools.
- Report every tool result, including capture/retrieval warnings, IDs, revisions, and doctor warnings.
