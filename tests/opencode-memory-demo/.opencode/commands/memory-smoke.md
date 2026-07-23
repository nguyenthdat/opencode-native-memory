---
description: Exercise native memory tools against the isolated demo project
agent: build
---

Run a non-destructive native-memory smoke test for this project:

1. Call `memory_status` and `memory_doctor`.
2. Call `memory_list` and verify the repository-scoped shared record about `src/app.ts` is present.
3. Store a project-scoped decision titled "Demo runtime" with content "The demo server listens on port 4317.", importance `0.8`, tags `demo` and `runtime`, taxonomy `decision`, and code path `src/app.ts`.
4. Search for "Which port does the demo server use?" and verify that decision is the top relevant result.
5. Fetch the full record, pin it, then unpin it using the latest `updated_at_ms`. Confirm each lifecycle mutation advances the revision without changing content.
6. Export a snapshot with expired and superseded records included. Report `format_version`, record count, and tombstone count, but do not import or purge automatically.
7. Run `memory_doctor` again and summarize backend/model/schema, retrieval warnings, lifecycle revisions, and any failures.

Do not delete, purge, promote, lock, or edit repository-scoped memory in this command.
