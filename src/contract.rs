use serde::{Deserialize, Serialize};

use crate::capture::{CaptureDecision, SourceTrust};
use crate::taxonomy::MemoryTaxonomy;

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Decision,
    Preference,
    Fact,
    Pattern,
    Gotcha,
    #[default]
    Summary,
}

impl MemoryKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Preference => "preference",
            Self::Fact => "fact",
            Self::Pattern => "pattern",
            Self::Gotcha => "gotcha",
            Self::Summary => "summary",
        }
    }

    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "decision" => Ok(Self::Decision),
            "preference" => Ok(Self::Preference),
            "fact" => Ok(Self::Fact),
            "pattern" => Ok(Self::Pattern),
            "gotcha" => Ok(Self::Gotcha),
            "summary" => Ok(Self::Summary),
            _ => anyhow::bail!("unknown memory kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Session,
    Agent,
    #[default]
    Project,
    Repository,
}

impl MemoryScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Repository => "repository",
        }
    }

    pub(crate) const fn precedence(self) -> u8 {
        match self {
            Self::Session => 4,
            Self::Agent => 3,
            Self::Project => 2,
            Self::Repository => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOrigin {
    #[default]
    Manual,
    AutoCompaction,
    SharedMarkdown,
    Legacy,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackEvent {
    Injected,
    Used,
    Ignored,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteReason {
    Obsolete,
    Incorrect,
    #[default]
    UserDeleted,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockAction {
    Lock,
    Unlock,
}

#[derive(Debug, Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeAnchor {
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub git_sha: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackStats {
    pub injected: u64,
    pub used: u64,
    pub ignored: u64,
    pub error: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScoreBreakdown {
    pub dense: f32,
    pub reciprocal_rank: f32,
    pub lexical: f32,
    pub channel_agreement: f32,
    pub calibrated: f32,
    pub retention: f32,
    pub feedback: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreRequest {
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: MemoryKind,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub scope: MemoryScope,
    #[serde(default)]
    pub scope_key: Option<String>,
    #[serde(default)]
    pub origin: MemoryOrigin,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    #[serde(default)]
    pub code_paths: Vec<String>,
    #[serde(default)]
    pub revive: bool,
    /// Phase 1 taxonomy override. When absent the engine infers it
    /// deterministically from `kind`, `scope`, and `code_paths`.
    #[serde(default)]
    pub taxonomy: Option<MemoryTaxonomy>,
    /// Phase 1 confidence override in `[0, 1]`. When absent the engine
    /// defaults to `importance` (capped at 0.6 for auto-compaction).
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub candidate: StoreRequest,
    pub significance: f32,
    pub impact: f32,
    pub rarity: f32,
    #[serde(default)]
    pub source_trust: SourceTrust,
    #[serde(default)]
    pub has_valid_evidence: bool,
    #[serde(default)]
    pub suggested_supersession_ids: Vec<String>,
    #[serde(default)]
    pub suggested_conflict_ids: Vec<String>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

const fn default_importance() -> f32 {
    0.7
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default = "default_budget_chars")]
    pub budget_chars: usize,
    #[serde(default)]
    pub kinds: Vec<MemoryKind>,
    #[serde(default)]
    pub scopes: Vec<MemoryScope>,
    /// Phase 1 taxonomy filter. When non-empty only memories whose taxonomy
    /// is in the list are eligible. Empty means no taxonomy filtering.
    #[serde(default)]
    pub taxonomies: Vec<MemoryTaxonomy>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    #[serde(default)]
    pub include_stale: bool,
    /// When `false` (the default) superseded memories are filtered out of
    /// search results. When `true` they are eligible alongside active ones.
    #[serde(default)]
    pub include_superseded: bool,
    #[serde(default)]
    pub track_feedback: bool,
}

const fn default_max_results() -> usize {
    20
}

const fn default_budget_chars() -> usize {
    6_000
}

const fn default_min_score() -> f32 {
    0.42
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GetRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    #[serde(default)]
    pub kinds: Vec<MemoryKind>,
    #[serde(default)]
    pub scopes: Vec<MemoryScope>,
    /// Phase 1 taxonomy filter. When non-empty only memories whose taxonomy
    /// is in the list are listed.
    #[serde(default)]
    pub taxonomies: Vec<MemoryTaxonomy>,
    #[serde(default)]
    pub include_expired: bool,
    #[serde(default)]
    pub include_stale: bool,
    /// When `false` (the default) superseded memories are excluded. When
    /// `true` they are listed alongside active ones.
    #[serde(default)]
    pub include_superseded: bool,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_list_limit")]
    pub limit: usize,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

const fn default_list_limit() -> usize {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    pub id: String,
    #[serde(default)]
    pub expected_updated_at_ms: Option<i64>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<MemoryKind>,
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub scope: Option<MemoryScope>,
    #[serde(default)]
    pub scope_key: Option<String>,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    #[serde(default)]
    pub clear_expiry: bool,
    #[serde(default)]
    pub code_paths: Option<Vec<String>>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub lock_action: Option<LockAction>,
    #[serde(default)]
    pub lock_reason: Option<String>,
    /// Phase 1 taxonomy override applied to the updated record. Changing the
    /// taxonomy of a locked or repository-scoped memory is rejected.
    #[serde(default)]
    pub taxonomy: Option<MemoryTaxonomy>,
    /// Phase 1 confidence override in `[0, 1]` applied to the updated record.
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Phase 1 explicit conflict-with list. The engine symmetrises links to
    /// every listed ID and caps participating confidence at 0.5. Passing an
    /// empty list clears the record's conflict links without restoring
    /// confidence. Relationship changes are rejected for locked/repository
    /// records. Direct supersession edits are not exposed through this
    /// request; supersession arises only from identity-changing updates.
    #[serde(default)]
    pub conflict_with: Option<Vec<String>>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    pub ids: Vec<String>,
    #[serde(default = "default_true")]
    pub tombstone: bool,
    #[serde(default)]
    pub reason: DeleteReason,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeRequest {
    pub project_id: String,
    #[serde(default = "default_true")]
    pub keep_tombstones: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    pub retrieval_id: String,
    pub event: FeedbackEvent,
    #[serde(default)]
    pub memory_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SharedMemoryInput {
    pub source: String,
    pub content: String,
    pub title: String,
    pub kind: MemoryKind,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub code_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncSharedRequest {
    pub records: Vec<SharedMemoryInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorRequest {
    #[serde(default)]
    pub deep: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryRecord {
    pub id: String,
    pub title: String,
    pub content: String,
    pub kind: MemoryKind,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub scope: MemoryScope,
    #[serde(default)]
    pub scope_key: Option<String>,
    pub origin: MemoryOrigin,
    pub expires_at_ms: Option<i64>,
    pub pinned: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub stale: bool,
    pub code_anchors: Vec<CodeAnchor>,
    pub feedback: FeedbackStats,
    /// Phase 1 taxonomy. Always populated on v3 records.
    pub taxonomy: MemoryTaxonomy,
    /// Phase 1 confidence in `[0, 1]`.
    pub confidence: f32,
    /// ID of the active successor that supersedes this memory, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// IDs of direct predecessors this memory supersedes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    /// IDs of memories that conflict with this memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_with: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_breakdown: Option<ScoreBreakdown>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    #[serde(default)]
    pub include_expired: bool,
    #[serde(default)]
    pub include_superseded: bool,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TombstoneSnapshot {
    pub fingerprint: String,
    pub kind: MemoryKind,
    pub scope: MemoryScope,
    #[serde(default)]
    pub scope_key: Option<String>,
    pub deleted_at_ms: i64,
    pub reason: DeleteReason,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshot {
    pub format_version: u32,
    pub source_project_id: String,
    pub exported_at_ms: i64,
    pub memories: Vec<MemoryRecord>,
    #[serde(default)]
    pub tombstones: Vec<TombstoneSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub snapshot: MemorySnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResponse {
    pub imported: usize,
    pub tombstones_imported: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoreResponse {
    pub id: String,
    pub inserted: bool,
    pub content_hash: String,
    pub updated_at_ms: i64,
    pub scope: MemoryScope,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureResponse {
    pub decision: CaptureDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored: Option<StoreResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub retrieval_id: Option<String>,
    pub count: usize,
    pub candidates_considered: usize,
    pub budget_chars: usize,
    pub used_chars: usize,
    pub abstained: bool,
    pub abstention_reason: Option<String>,
    pub score_version: &'static str,
    pub warnings: Vec<String>,
    pub memories: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListResponse {
    pub total: usize,
    pub offset: usize,
    pub count: usize,
    pub memories: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
    pub updated_at_ms: i64,
}

/// Phase 1 dedicated pin request sharing `update`'s optimistic-concurrency
/// and scope-authorization semantics.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PinRequest {
    pub id: String,
    pub pinned: bool,
    #[serde(default)]
    pub expected_updated_at_ms: Option<i64>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

/// Phase 1 dedicated lock request sharing `update`'s optimistic-concurrency
/// and scope-authorization semantics.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockRequest {
    pub id: String,
    pub lock_action: LockAction,
    #[serde(default)]
    pub lock_reason: Option<String>,
    #[serde(default)]
    pub expected_updated_at_ms: Option<i64>,
    #[serde(default)]
    pub session_scope_key: Option<String>,
    #[serde(default)]
    pub agent_scope_key: Option<String>,
}

/// Phase 1 lifecycle response returned by dedicated `pin` and `lock` RPCs.
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleResponse {
    pub id: String,
    pub pinned: bool,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteResponse {
    pub requested: usize,
    pub deleted: u64,
    pub tombstones_created: usize,
}

pub type ForgetResponse = DeleteResponse;

#[derive(Debug, Clone, Serialize)]
pub struct PurgeResponse {
    pub deleted: u64,
    pub tombstones_retained: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackResponse {
    pub retrieval_id: String,
    pub recorded: bool,
    pub affected: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncSharedResponse {
    pub imported: usize,
    pub removed: usize,
    pub rejected: usize,
    pub rejected_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub name: String,
    pub completeness: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub ready: bool,
    pub rpc_protocol_version: u32,
    pub backend: String,
    pub zvec_version: String,
    pub embedding_model: String,
    pub embedding_dimension: usize,
    pub project_root: String,
    pub project_id: String,
    pub collection_path: String,
    pub document_count: u64,
    pub state_schema_version: u32,
    pub metadata_count: usize,
    pub tombstone_count: usize,
    pub retrieval_count: usize,
    pub pending_upsert_count: usize,
    pub pending_delete_count: usize,
    pub indexes: Vec<IndexStatus>,
    /// Phase 1 capability strings advertised to the TypeScript client. The
    /// current set is `["phase1_taxonomy_lifecycle_v1"]`.
    pub capabilities: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizeResponse {
    pub optimized: bool,
    pub document_count: u64,
    pub pruned_expired: usize,
    pub pruned_retrievals: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorResponse {
    pub ok: bool,
    pub project_root: String,
    pub project_id: String,
    pub collection_path: String,
    pub state_path: String,
    pub model_cache: String,
    pub document_count: u64,
    pub metadata_count: usize,
    pub stale_count: usize,
    pub expired_count: usize,
    pub tombstone_count: usize,
    pub retrieval_count: usize,
    pub pending_upsert_count: usize,
    pub pending_delete_count: usize,
    pub git_sha: Option<String>,
    pub warnings: Vec<String>,
}
