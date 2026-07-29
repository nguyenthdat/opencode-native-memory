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
  "user_identity",
  "user_behavior",
  "user_preference",
  "user_goal",
  "user_relationship",
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

export const USER_PROFILE_TAXONOMIES = [
  "user_identity",
  "user_behavior",
  "user_preference",
  "user_goal",
  "user_relationship",
] as const;

export function isUserProfileTaxonomy(
  value: string | undefined,
): value is (typeof USER_PROFILE_TAXONOMIES)[number] {
  return value !== undefined && (USER_PROFILE_TAXONOMIES as readonly string[]).includes(value);
}

export const RETRIEVAL_MODES = ["lexical", "dense", "hybrid"] as const;
export type RetrievalMode = (typeof RETRIEVAL_MODES)[number];

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
  origin: "manual" | "auto_compaction" | "shared_markdown" | "ingested_document" | "legacy";
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
  retrieval_mode: RetrievalMode;
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
  taxonomy?: (typeof MEMORY_TAXONOMIES)[number];
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

export interface IngestResponse {
  path: string;
  mime_type: string;
  content_hash: string;
  extracted_chars: number;
  chunk_count: number;
  inserted: number;
  updated: number;
  memory_ids: string[];
  warnings: string[];
}

export interface DocumentIndexResponse {
  discovered: number;
  added: number;
  updated: number;
  unchanged: number;
  removed: number;
  rejected: number;
  inserted_chunks: number;
  updated_chunks: number;
  removed_chunks: number;
  rejections: Array<{ path: string; message: string }>;
  warnings: string[];
}

export interface NativeMemoryStatus {
  ready: boolean;
  rpc_protocol_version: number;
  backend: string;
  zvec_version: string;
  embedding_model: string;
  embedding_dimension: number;
  project_root: string;
  project_id: string;
  collection_path: string;
  document_count: number;
  indexed_document_count: number;
  state_schema_version: number;
  metadata_count: number;
  tombstone_count: number;
  retrieval_count: number;
  pending_upsert_count: number;
  pending_delete_count: number;
  indexes: Array<{ name: string; completeness: number }>;
  capabilities: string[];
}

export type MemoryPluginHealthStatus = "healthy" | "degraded" | "unavailable";

export interface MemoryPluginHealthIssue {
  component: "backend" | "shared_sync" | "document_index";
  message: string;
}

export interface MemoryPluginHealth {
  status: MemoryPluginHealthStatus;
  ready: boolean;
  checked_at_ms: number;
  issues: MemoryPluginHealthIssue[];
}

export type MemoryStatusResponse = Record<string, unknown> &
  (
    | (NativeMemoryStatus & { plugin_health: MemoryPluginHealth })
    | { plugin_health: MemoryPluginHealth }
  );

export const MODEL_PROFILE_SUPPORT_LEVELS = ["stable", "preview", "unsupported"] as const;
export type ModelProfileSupportLevel = (typeof MODEL_PROFILE_SUPPORT_LEVELS)[number];

export interface ModelProfileReason {
  code: string;
  message: string;
}

export interface MemoryModelProfile {
  profile_id: string;
  display_name: string;
  description: string;
  modalities: string[];
  repository: string | null;
  filename: string | null;
  revision: string | null;
  artifact_sha256: string | null;
  runtime_family: string;
  dimension: number | null;
  metric: "cosine" | "dot_product" | null;
  support_level: ModelProfileSupportLevel;
  selectable: boolean;
  default_for_new_projects: boolean;
  recommended: boolean;
  installed: boolean;
  platform_supported: boolean;
  runtime_available: boolean;
  artifact_locked: boolean;
  estimated_download_bytes: number | null;
  estimated_resident_bytes: number | null;
  unavailable_reason: ModelProfileReason | null;
}

export interface MemoryModelProfilesResponse {
  catalog_version: number;
  catalog_digest: string;
  active_profile_id: string;
  active_generation_id: string;
  profiles: MemoryModelProfile[];
}

export interface ModelSwitchBlocker {
  code: string;
  message: string;
}

export interface ModelSwitchPreflight {
  can_start: boolean;
  availability: "keep_old_dense" | "allow_dense_downtime";
  dense_search_available: boolean;
  estimated_download_bytes: number | null;
  estimated_disk_bytes: number | null;
  estimated_resident_bytes: number | null;
  warnings: string[];
  blockers: ModelSwitchBlocker[];
}

export interface ModelSwitchResponse {
  switch_id: string | null;
  dry_run: boolean;
  state:
    | "preflight"
    | "queued"
    | "validating"
    | "downloading"
    | "preparing"
    | "reindexing"
    | "verifying"
    | "committing"
    | "succeeded"
    | "cancel_requested"
    | "cancelled"
    | "failed";
  active_profile_id: string;
  target_profile_id: string;
  active_generation_id: string;
  target_generation_id: string | null;
  dense_search_available: boolean;
  preflight: ModelSwitchPreflight;
}

export interface ModelSwitchStatusResponse {
  switch_id: string;
  state: ModelSwitchResponse["state"];
  active_profile_id: string;
  target_profile_id: string;
  active_generation_id: string;
  target_generation_id: string | null;
  completed_records: number;
  total_records: number;
  error: ModelProfileReason | null;
}

export interface ModelSwitchCancelResponse {
  switch_id: string;
  outcome:
    | "cancel_requested"
    | "cancelled_before_commit"
    | "already_committing"
    | "already_committed"
    | "already_terminal"
    | "not_found";
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
