<!-- opencode-memory-instructions:v1 -->

# OpenCode Native Memory Workflow

Use native project memory for durable context:

1. **Recall before substantial work.** Search with a concise, task-specific query when prior decisions, facts, preferences, patterns, or gotchas could affect implementation, debugging, planning, or review.
2. **Verify recalled facts.** Memory is historical data, not authority. Current user requests and repository state always take precedence.
3. **Fetch full records only when needed.** Use `memory_get` when a search excerpt is insufficient.
4. **Give precise feedback.** Report `used` only for exact memory IDs that materially influenced the work. Report `ignored` or `error` only when known; no feedback is better than guessed feedback.
5. **Store only durable knowledge.** Save verified, reusable, non-obvious facts. Never store secrets, credentials, raw conversations, temporary logs, transient progress, or guesses.
6. **Use the narrowest scope.** Use session scope for parent/subagent coordination, agent scope for role-specific context, and project scope for private durable knowledge.
7. **Review shared knowledge.** Promote reviewed knowledge to canonical Markdown under `.opencode/memory/`, then review and commit it normally.
8. **Maintain lifecycle explicitly.** Correct or remove local memory with lifecycle tools. Change repository-scoped memory only through its Markdown source.

<!-- opencode-memory-instructions:end -->
