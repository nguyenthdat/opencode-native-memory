export const MEMORY_KINDS = [
  "decision",
  "preference",
  "fact",
  "pattern",
  "gotcha",
  "summary",
] as const;

export const MEMORY_SCOPES = ["session", "agent", "project", "repository"] as const;

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

export interface SharedMemoryRecord extends CuratedCandidate {
  source: string;
}

export interface SharedSyncResponse {
  imported: number;
  removed: number;
  rejected: number;
  rejected_sources: string[];
}

export interface RpcResponse {
  id: number;
  ok: boolean;
  result?: unknown;
  error?: string;
}

export interface PendingRequest {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
  abort?: () => void;
  signal?: AbortSignal;
}
