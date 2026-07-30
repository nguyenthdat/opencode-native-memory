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
  score_breakdown?: {
    dense: number;
    reciprocal_rank: number;
    lexical: number;
    channel_agreement: number;
    calibrated: number;
    retention: number;
    feedback: number;
    graph_rrf?: number;
  };
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

export interface GraphAuthorization {
  readonly session_scope_key: string;
  readonly agent_scope_key: string;
}

export interface GraphScopeFilter {
  readonly memory_scope?: (typeof MEMORY_SCOPES)[number];
  readonly verified_scope_key?: string;
}

export interface GraphTimeFilter {
  /** Inclusive valid-at bounds. Equal bounds represent an exact as-of instant. */
  readonly valid_after_ms?: number;
  readonly valid_before_ms?: number;
  readonly extracted_after_ms?: number;
  readonly extracted_before_ms?: number;
}

export interface GraphSearchRequest {
  readonly authorization: GraphAuthorization;
  readonly query: string;
  readonly scope?: GraphScopeFilter;
  readonly time?: GraphTimeFilter;
  readonly max_depth: number;
  readonly max_fanout: number;
  readonly max_results: number;
  readonly max_evidence_per_fact: number;
}

export interface GraphStatusRequest {
  readonly authorization: GraphAuthorization;
  readonly scope?: GraphScopeFilter;
}

export interface GraphExportRequest {
  readonly authorization: GraphAuthorization;
  readonly scope?: GraphScopeFilter;
  readonly cursor?: string;
  readonly page_limit: number;
}

export interface GraphDerivedScope {
  readonly project_id: string;
  readonly memory_scope: string;
  readonly verified_scope_key: string;
}

export interface GraphCandidateEvidence {
  readonly source_unit_id: string;
  readonly quote: string;
  readonly utf8_start?: number;
  readonly utf8_end?: number;
  readonly occurrence_index: number;
}

export interface GraphSourceBinding {
  readonly source_memory_id: string;
  readonly source_unit_id: string;
  readonly content_hash: string;
  readonly extraction_revision: string;
  readonly derived_scope?: GraphDerivedScope;
  readonly origin: string;
  readonly policy_revision: string;
  readonly remote_eligible: boolean;
}

export interface GraphExtractionUnit {
  readonly source?: GraphSourceBinding;
  readonly text: string;
  readonly remote_ineligible_reason?: string;
}

export interface GraphRejectedSource {
  readonly source_memory_id: string;
  readonly code: string;
  readonly message: string;
}

export interface GraphProviderIdentity {
  readonly provider_id: string;
  readonly model_id: string;
  readonly extractor_version: string;
  readonly prompt_version: string;
  readonly schema_version: string;
  readonly variant?: string;
}

export interface GraphRunReceipt {
  readonly extraction_run_id: string;
  readonly idempotency_digest: string;
  readonly outcome: string;
  readonly committed_at_ms: number;
  readonly source_count: number;
  readonly accepted_entity_count: number;
  readonly accepted_relation_count: number;
  readonly rejected_candidate_count: number;
  readonly conflict_count: number;
  readonly warning_count: number;
  readonly terminal: boolean;
}

export interface GraphUpsertCandidatesResponse {
  readonly receipt?: GraphRunReceipt;
  readonly accepted_entities: readonly unknown[];
  readonly accepted_relations: readonly unknown[];
  readonly rejected_candidates: readonly unknown[];
  readonly conflicts: readonly unknown[];
  readonly warnings: readonly string[];
}

export type GraphExtractionJobState =
  "queued" | "claimed" | "running" | "completed" | "failed" | "cancelled";

export interface GraphExtractionJob {
  readonly job_id: string;
  readonly idempotency_digest: string;
  readonly state: GraphExtractionJobState;
  readonly sources: readonly GraphSourceBinding[];
  readonly provider?: GraphProviderIdentity;
  readonly attempt_count: number;
  readonly max_attempts: number;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
  readonly lease_expires_at_ms?: number;
  readonly extraction_run_id: string;
  readonly next_attempt_at_ms?: number;
  readonly cancel_requested: boolean;
  readonly error_code: string;
  readonly error_message: string;
  readonly max_unit_text_bytes: number;
  readonly max_total_text_bytes: number;
}

export interface GraphExtractEnqueueResponse {
  readonly job?: GraphExtractionJob;
  readonly existing: boolean;
  readonly rejected_sources: readonly GraphRejectedSource[];
  readonly warnings: readonly string[];
}

export interface GraphExtractClaimResponse {
  readonly found: boolean;
  readonly job?: GraphExtractionJob;
  readonly lease_token: string;
  readonly units: readonly GraphExtractionUnit[];
  readonly rejected_sources: readonly GraphRejectedSource[];
  readonly warnings: readonly string[];
}

export interface GraphExtractRenewResponse {
  readonly job?: GraphExtractionJob;
  readonly lease_expires_at_ms: number;
  readonly cancel_requested: boolean;
}

export interface GraphExtractFinishResponse {
  readonly job?: GraphExtractionJob;
  readonly upsert?: GraphUpsertCandidatesResponse;
  readonly warnings: readonly string[];
}

export interface GraphExtractJobStatusResponse {
  readonly found: boolean;
  readonly job?: GraphExtractionJob;
}

export interface GraphExtractCancelResponse {
  readonly job?: GraphExtractionJob;
  readonly outcome: "cancelled" | "cancel_requested" | "already_terminal";
}

export interface GraphEvidenceProvenance {
  readonly source_memory_id: string;
  readonly source_unit_id: string;
  readonly content_hash: string;
  readonly extraction_revision: string;
  readonly derived_scope?: GraphDerivedScope;
  readonly evidence: readonly GraphCandidateEvidence[];
}

export interface GraphEntity {
  readonly entity_id: string;
  readonly canonical_name: string;
  readonly entity_type: string;
  readonly aliases: readonly string[];
  readonly derived_scope?: GraphDerivedScope;
  readonly first_seen_at_ms: number;
  readonly last_seen_at_ms: number;
  readonly source_count: number;
}

export interface GraphRelation {
  readonly relation_id: string;
  readonly subject_entity_id: string;
  readonly predicate: string;
  readonly object_entity_id: string;
  readonly relation_type: string;
  readonly valid_at_ms?: number;
  readonly invalid_at_ms?: number;
  readonly created_at_ms: number;
  readonly extracted_at_ms: number;
  readonly confidence: number;
  readonly status: string;
  readonly source_memory_ids: readonly string[];
  readonly evidence: readonly GraphCandidateEvidence[];
  readonly extractor_version: string;
  readonly derived_scope?: GraphDerivedScope;
}

export interface GraphScoreComponent {
  readonly name: string;
  readonly value: number;
}

export interface GraphMemorySearchResult {
  readonly source_memory_id: string;
  readonly score: number;
  readonly provenance: readonly GraphEvidenceProvenance[];
  readonly score_trace: readonly GraphScoreComponent[];
}

export interface GraphEntitySearchResult {
  readonly entity?: GraphEntity;
  readonly score: number;
  readonly provenance: readonly GraphEvidenceProvenance[];
  readonly score_trace: readonly GraphScoreComponent[];
}

export interface GraphRelationSearchResult {
  readonly relation?: GraphRelation;
  readonly score: number;
  readonly provenance: readonly GraphEvidenceProvenance[];
  readonly score_trace: readonly GraphScoreComponent[];
}

export interface GraphSearchResponse {
  readonly memories: readonly GraphMemorySearchResult[];
  readonly entities: readonly GraphEntitySearchResult[];
  readonly relations: readonly GraphRelationSearchResult[];
  readonly eligible_source_count: number;
  readonly truncated: boolean;
}

export interface GraphLastExtraction {
  readonly extraction_run_id: string;
  readonly completed_at_ms: number;
  readonly source_count: number;
}

export interface GraphStatusResponse {
  readonly schema_version: string;
  readonly entity_count: number;
  readonly relation_count: number;
  readonly pending_job_count: number;
  readonly last_extraction?: GraphLastExtraction;
}

export interface GraphExportProvenance {
  readonly fact_kind: string;
  readonly fact_id: string;
  readonly sources: readonly GraphEvidenceProvenance[];
}

export interface GraphExportResponse {
  readonly schema_version: string;
  readonly entities: readonly GraphEntity[];
  readonly relations: readonly GraphRelation[];
  readonly provenance: readonly GraphExportProvenance[];
  readonly next_cursor?: string;
  readonly complete: boolean;
}

export interface NativeMemoryStatus {
  ready: boolean;
  rpc_protocol_version: number;
  backend: string;
  zvec_version: string;
  embedding_model: string;
  embedding_dimension: number;
  active_profile_id: string;
  active_generation_id: string;
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

export interface OptimizeResponse {
  optimized: boolean;
  document_count: number;
  pruned_expired: number;
  pruned_retrievals: number;
  indexes: Array<{ name: string; completeness: number }>;
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
