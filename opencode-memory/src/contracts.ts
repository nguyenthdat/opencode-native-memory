export const MEMORY_KINDS = [
  "decision",
  "preference",
  "fact",
  "pattern",
  "gotcha",
  "summary",
] as const;

export const MEMORY_SCOPES = ["session", "agent", "project", "repository"] as const;

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
] as const;

export const WRITABLE_MEMORY_SCOPES = ["session", "agent", "project"] as const;

export const FEEDBACK_EVENTS = ["used", "ignored", "error"] as const;

export const LOCK_ACTIONS = ["lock", "unlock"] as const;

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
] as const;

export interface MemoryRecord {
  id: string;
  title: string;
  content: string;
  kind: (typeof MEMORY_KINDS)[number];
  importance: number;
  tags: string[];
  source: string;
  created_at_ms: number;
  updated_at_ms: number;
  scope: (typeof MEMORY_SCOPES)[number];
  scope_key?: string | null;
  origin: "manual" | "auto_compaction" | "shared_markdown" | "legacy";
  expires_at_ms?: number | null;
  stale: boolean;
  code_anchors: Array<{ path: string; sha256: string; git_sha?: string }>;
  feedback: {
    injected: number;
    used: number;
    ignored: number;
    error: number;
  };
  score?: number;
  pinned: boolean;
  locked: boolean;
  lock_reason?: string | null;
  taxonomy: (typeof MEMORY_TAXONOMIES)[number];
  confidence: number;
  superseded_by?: string | null;
  supersedes?: string[];
  conflict_with?: string[];
}

export interface SearchResponse {
  query: string;
  retrieval_id?: string | null;
  count: number;
  candidates_considered: number;
  budget_chars: number;
  used_chars: number;
  abstained: boolean;
  abstention_reason?: string | null;
  score_version: string;
  warnings: string[];
  memories: MemoryRecord[];
}

export interface ListResponse {
  total: number;
  offset: number;
  count: number;
  memories: MemoryRecord[];
}

export interface PendingRecall {
  retrievalID: string;
  memoryIDs: string[];
}

export interface CuratedCandidate {
  title: string;
  content: string;
  kind: Exclude<(typeof MEMORY_KINDS)[number], "summary">;
  importance: number;
  tags: string[];
  code_paths: string[];
}

export interface CaptureResponse {
  decision: {
    outcome: "skip" | "quarantine" | "accept";
    reason?: string;
    confidence?: number;
    confidence_capped?: boolean;
  };
  stored?: {
    id: string;
    inserted: boolean;
    content_hash: string;
    updated_at_ms: number;
    scope: (typeof MEMORY_SCOPES)[number];
  };
}

export interface SharedMemoryRecord extends CuratedCandidate {
  source: string;
}

export interface SharedMemoryLoadError {
  source: string;
  message: string;
}

export interface SharedMemoryLoadResult {
  records: SharedMemoryRecord[];
  signature: string;
  errors: SharedMemoryLoadError[];
}

export interface SharedSyncResponse {
  imported: number;
  removed: number;
  rejected: number;
  rejections: SharedMemoryLoadError[];
}

export interface RpcResponse {
  id: number;
  ok: boolean;
  result?: unknown | undefined;
  error?: string | undefined;
}

export interface PendingRequest {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
  abort?: (() => void) | undefined;
  signal?: AbortSignal | undefined;
}
