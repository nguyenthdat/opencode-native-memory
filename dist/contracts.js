export const MEMORY_KINDS = [
    "decision",
    "preference",
    "fact",
    "pattern",
    "gotcha",
    "summary",
];
export const MEMORY_SCOPES = ["session", "agent", "project", "repository"];
export const MEMORY_TAXONOMIES = [
    "task_attempt",
    "tool_call",
    "session_summary",
    "architecture_fact",
    "codebase_fact",
    "user_fact",
    "fix_pattern",
    "code_template",
    "tool_heuristic",
    "code_style",
    "library_pref",
    "workflow_pref",
    "decision",
    "team_convention",
    "project_standard",
];
export const WRITABLE_MEMORY_SCOPES = ["session", "agent", "project"];
export const FEEDBACK_EVENTS = ["used", "ignored", "error"];
export const LOCK_ACTIONS = ["lock", "unlock"];
export const LOCK_REASON_MAX = 240;
export const UNLOCK_FORBIDDEN_FIELDS = [
    "content",
    "title",
    "kind",
    "importance",
    "tags",
    "scope",
    "expires_in_days",
    "clear_expiry",
    "code_paths",
    "pinned",
    "taxonomy",
    "confidence",
    "conflict_with",
];
//# sourceMappingURL=contracts.js.map