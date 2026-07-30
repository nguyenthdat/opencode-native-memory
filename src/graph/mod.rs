//! Actor-owned, source-backed knowledge graph storage.
//!
//! The graph is deliberately a derived sidecar. Memory documents and lifecycle
//! state remain authoritative; every graph fact keeps source evidence and is
//! rechecked by `MemoryEngine` before it is exposed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::config::hash_hex;
use crate::storage::atomic::{remove_file_durable, write_json_atomic};

pub(crate) const GRAPH_SCHEMA_VERSION: u32 = 1;
pub(crate) const GRAPH_POLICY_VERSION: &str = "egress-v1";
pub(crate) const ENTITY_NORMALIZATION_VERSION: &str = "nfkc-lowercase-whitespace-v1";
pub(crate) const ENTITY_RESOLUTION_VERSION: &str = "exact-alias-token-jaccard-0.9-v1";
pub(crate) const GRAPH_STATE_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const GRAPH_PENDING_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_GRAPH_UNITS: usize = 64;
pub(crate) const MAX_GRAPH_ENTITIES: usize = 64;
pub(crate) const MAX_GRAPH_RELATIONS: usize = 64;
pub(crate) const MAX_GRAPH_RESULTS: usize = 64;
pub(crate) const MAX_GRAPH_EVIDENCE: usize = 8;
pub(crate) const MAX_GRAPH_FANOUT: usize = 32;
pub(crate) const MAX_GRAPH_DEPTH: usize = 2;
pub(crate) const MAX_GRAPH_PAGE: usize = 100;
pub(crate) const MAX_GRAPH_TEXT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_GRAPH_TOTAL_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_GRAPH_STRING_CHARS: usize = 512;
pub(crate) const MAX_GRAPH_QUOTE_CHARS: usize = 1_024;
const MAX_GRAPH_JOB_ERROR_CHARS: usize = 2_048;
pub(crate) const MAX_GRAPH_JOBS: usize = 1_024;
pub(crate) const MAX_GRAPH_JOB_ATTEMPTS: u32 = 5;
pub(crate) const DEFAULT_GRAPH_JOB_ATTEMPTS: u32 = 3;
pub(crate) const MIN_GRAPH_JOB_LEASE_MS: u32 = 5_000;
pub(crate) const DEFAULT_GRAPH_JOB_LEASE_MS: u32 = 60_000;
pub(crate) const MAX_GRAPH_JOB_LEASE_MS: u32 = 5 * 60_000;
const GRAPH_JOB_RETRY_BASE_MS: u64 = 1_000;
const MAX_GRAPH_JOB_RETRY_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphScopeKind {
    Project,
    Repository,
    Agent,
    Session,
}

impl GraphScopeKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Repository => "repository",
            Self::Agent => "agent",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphScope {
    pub project_id: String,
    pub kind: GraphScopeKind,
    #[serde(default)]
    pub scope_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphSource {
    pub source_memory_id: String,
    pub source_unit_id: String,
    pub content_hash: String,
    pub extraction_revision: String,
    pub scope: GraphScope,
    pub origin: String,
    pub policy_revision: String,
    pub remote_eligible: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphEvidence {
    pub source_memory_id: String,
    pub source_unit_id: String,
    pub content_hash: String,
    pub extraction_revision: String,
    pub scope: GraphScope,
    pub quote: String,
    pub occurrence_index: u32,
    #[serde(default)]
    pub utf8_start: Option<u32>,
    #[serde(default)]
    pub utf8_end: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphEntity {
    pub entity_id: String,
    pub canonical_name: String,
    pub normalized_name: String,
    pub entity_type: String,
    #[serde(default)]
    pub aliases: BTreeSet<String>,
    pub scope: GraphScope,
    pub first_seen_at_ms: u64,
    pub last_seen_at_ms: u64,
    pub source_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphRelation {
    pub relation_id: String,
    pub subject_entity_id: String,
    pub predicate: String,
    pub object_entity_id: String,
    pub relation_type: String,
    #[serde(default)]
    pub valid_at_ms: Option<u64>,
    #[serde(default)]
    pub invalid_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub extracted_at_ms: u64,
    pub confidence: f64,
    pub status: String,
    #[serde(default)]
    pub evidence: Vec<GraphEvidence>,
    pub extractor_version: String,
    pub scope: GraphScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphMention {
    pub entity_id: String,
    pub evidence: GraphEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphRun {
    pub extraction_run_id: String,
    pub idempotency_digest: String,
    pub outcome: String,
    pub committed_at_ms: u64,
    pub source_count: u64,
    pub accepted_entity_count: u64,
    pub accepted_relation_count: u64,
    pub rejected_candidate_count: u64,
    pub conflict_count: u64,
    pub warning_count: u64,
    pub terminal: bool,
    pub provider_id: String,
    pub model_id: String,
    pub extractor_version: String,
    pub prompt_version: String,
    pub candidate_schema_version: String,
    #[serde(default)]
    pub scopes: Vec<GraphScope>,
    #[serde(default)]
    pub source_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphJobState {
    Queued,
    Claimed,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl GraphJobState {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub(crate) const fn is_active(self) -> bool {
        matches!(self, Self::Claimed | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphJobSource {
    pub source_memory_id: String,
    pub source_unit_id: String,
    pub content_hash: String,
    pub extraction_revision: String,
    pub scope: GraphScope,
    pub origin: String,
    pub policy_revision: String,
    pub remote_eligible: bool,
}

impl From<&GraphSource> for GraphJobSource {
    fn from(source: &GraphSource) -> Self {
        Self {
            source_memory_id: source.source_memory_id.clone(),
            source_unit_id: source.source_unit_id.clone(),
            content_hash: source.content_hash.clone(),
            extraction_revision: source.extraction_revision.clone(),
            scope: source.scope.clone(),
            origin: source.origin.clone(),
            policy_revision: source.policy_revision.clone(),
            remote_eligible: source.remote_eligible,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphExtractionJob {
    pub job_id: String,
    pub idempotency_digest: String,
    pub state: GraphJobState,
    #[serde(default)]
    pub sources: Vec<GraphJobSource>,
    pub provider: GraphProvider,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub lease_expires_at_ms: Option<u64>,
    #[serde(default)]
    pub lease_token: Option<String>,
    #[serde(default)]
    pub claim_request_id: Option<String>,
    #[serde(default)]
    pub worker_id: Option<String>,
    pub extraction_run_id: String,
    #[serde(default)]
    pub next_attempt_at_ms: Option<u64>,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub error_code: String,
    #[serde(default)]
    pub error_message: String,
    pub max_unit_text_bytes: u32,
    pub max_total_text_bytes: u32,
    #[serde(default)]
    pub completion_digest: Option<String>,
    #[serde(default)]
    pub completion_lease_token_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphState {
    pub schema_version: u32,
    pub generation: u64,
    pub policy_revision: String,
    pub normalization_version: String,
    pub resolution_version: String,
    #[serde(default)]
    pub entities: BTreeMap<String, GraphEntity>,
    #[serde(default)]
    pub relations: BTreeMap<String, GraphRelation>,
    #[serde(default)]
    pub mentions: Vec<GraphMention>,
    #[serde(default)]
    pub runs: BTreeMap<String, GraphRun>,
    #[serde(default)]
    pub jobs: BTreeMap<String, GraphExtractionJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingGraph {
    schema_version: u32,
    transaction_id: String,
    state: GraphState,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            schema_version: GRAPH_SCHEMA_VERSION,
            generation: 0,
            policy_revision: GRAPH_POLICY_VERSION.to_string(),
            normalization_version: ENTITY_NORMALIZATION_VERSION.to_string(),
            resolution_version: ENTITY_RESOLUTION_VERSION.to_string(),
            entities: BTreeMap::new(),
            relations: BTreeMap::new(),
            mentions: Vec::new(),
            runs: BTreeMap::new(),
            jobs: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GraphStore {
    path: std::path::PathBuf,
    pending_path: std::path::PathBuf,
    state: GraphState,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEntityInput {
    pub mention: String,
    pub canonical_hint: String,
    pub entity_type: String,
    pub aliases: Vec<String>,
    pub evidence: Vec<GraphEvidenceInput>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphRelationInput {
    pub subject_mention: String,
    pub predicate: String,
    pub object_mention: String,
    pub relation_type: String,
    pub valid_at_ms: Option<u64>,
    pub invalid_at_ms: Option<u64>,
    pub evidence: Vec<GraphEvidenceInput>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEvidenceInput {
    pub source_unit_id: String,
    pub quote: String,
    pub utf8_start: Option<u32>,
    pub utf8_end: Option<u32>,
    pub occurrence_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GraphProvider {
    pub provider_id: String,
    pub model_id: String,
    pub extractor_version: String,
    pub prompt_version: String,
    pub schema_version: String,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphUpsertOutcome {
    pub run: GraphRun,
    pub entities: Vec<(usize, GraphEntity)>,
    pub relations: Vec<(usize, GraphRelation)>,
    pub rejected: Vec<GraphCandidateRejection>,
    pub conflicts: Vec<GraphCandidateConflict>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphJobFinishOutcome {
    Completed,
    RetryableFailure,
    PermanentFailure,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphJobClaim {
    pub job: GraphExtractionJob,
    pub lease_token: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphJobFinishResult {
    pub job: GraphExtractionJob,
    pub upsert: Option<GraphUpsertOutcome>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphCandidateRejection {
    pub kind: String,
    pub index: usize,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphCandidateConflict {
    pub kind: String,
    pub index: usize,
    pub existing_id: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphSearchResult {
    pub memory_id: String,
    pub score: f64,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
    pub evidence: Vec<GraphEvidence>,
}

impl GraphStore {
    pub(crate) fn load(path: &Path, pending_path: &Path) -> Result<Self> {
        let mut state = if path.exists() {
            let bytes = fs::read(path)
                .with_context(|| format!("cannot read knowledge graph {}", path.display()))?;
            ensure!(
                bytes.len() <= GRAPH_STATE_MAX_BYTES,
                "knowledge graph state exceeds size limit"
            );
            serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid knowledge graph {}", path.display()))?
        } else {
            GraphState::default()
        };
        validate_state(&state)?;

        if pending_path.exists() {
            let bytes = fs::read(pending_path).with_context(|| {
                format!(
                    "cannot read pending knowledge graph {}",
                    pending_path.display()
                )
            })?;
            ensure!(
                bytes.len() <= GRAPH_PENDING_MAX_BYTES,
                "pending knowledge graph transaction exceeds size limit"
            );
            let pending: PendingGraph = serde_json::from_slice(&bytes)
                .context("invalid pending knowledge graph transaction")?;
            ensure!(
                pending.schema_version == GRAPH_SCHEMA_VERSION,
                "unsupported pending graph schema"
            );
            validate_state(&pending.state)?;
            if pending.state.generation > state.generation {
                write_json_atomic(path, &pending.state, GRAPH_STATE_MAX_BYTES)?;
                state = pending.state;
            }
            remove_file_durable(pending_path)?;
        }

        let mut store = Self {
            path: path.to_path_buf(),
            pending_path: pending_path.to_path_buf(),
            state,
        };
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before Unix epoch")?
                .as_millis(),
        )
        .context("graph timestamp exceeds u64")?;
        store.recover_job_leases(now_ms)?;
        Ok(store)
    }

    pub(crate) fn state(&self) -> &GraphState {
        &self.state
    }

    pub(crate) fn entity(&self, id: &str) -> Option<&GraphEntity> {
        self.state.entities.get(id)
    }

    pub(crate) fn relation(&self, id: &str) -> Option<&GraphRelation> {
        self.state.relations.get(id)
    }

    pub(crate) fn run(&self, id: &str) -> Option<&GraphRun> {
        self.state.runs.get(id)
    }

    pub(crate) fn job(&self, id: &str) -> Option<&GraphExtractionJob> {
        self.state.jobs.get(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enqueue_job(
        &mut self,
        job_id: &str,
        sources: &[GraphSource],
        provider: &GraphProvider,
        max_attempts: u32,
        max_unit_text_bytes: u32,
        max_total_text_bytes: u32,
        now_ms: u64,
    ) -> Result<(GraphExtractionJob, bool)> {
        validate_job_id(job_id)?;
        validate_provider(provider)?;
        ensure!(
            !sources.is_empty() && sources.len() <= MAX_GRAPH_UNITS,
            "graph job source count is invalid"
        );
        ensure!(
            sources.iter().all(|source| source.remote_eligible),
            "graph job contains a remote-ineligible source"
        );
        ensure!(
            (1..=MAX_GRAPH_JOB_ATTEMPTS).contains(&max_attempts),
            "graph job attempt limit is invalid"
        );
        ensure!(
            (1..=MAX_GRAPH_TEXT_BYTES as u32).contains(&max_unit_text_bytes),
            "graph job unit text limit is invalid"
        );
        ensure!(
            (max_unit_text_bytes..=MAX_GRAPH_TOTAL_TEXT_BYTES as u32)
                .contains(&max_total_text_bytes),
            "graph job total text limit is invalid"
        );
        let digest = job_digest(
            job_id,
            sources,
            provider,
            max_attempts,
            max_unit_text_bytes,
            max_total_text_bytes,
        )?;
        if let Some(existing) = self.state.jobs.get(job_id) {
            ensure!(
                existing.idempotency_digest == digest,
                "graph job ID was already used with different material"
            );
            return Ok((existing.clone(), true));
        }
        let mut next = self.state.clone();
        prune_terminal_jobs(&mut next);
        ensure!(
            next.jobs.len() < MAX_GRAPH_JOBS,
            "graph job count exceeds limit"
        );
        let job = GraphExtractionJob {
            job_id: job_id.to_string(),
            idempotency_digest: digest,
            state: GraphJobState::Queued,
            sources: sources.iter().map(GraphJobSource::from).collect(),
            provider: provider.clone(),
            attempt_count: 0,
            max_attempts,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            lease_expires_at_ms: None,
            lease_token: None,
            claim_request_id: None,
            worker_id: None,
            extraction_run_id: format!("run_{}", &hash_hex(job_id.as_bytes())[..32]),
            next_attempt_at_ms: None,
            cancel_requested: false,
            error_code: String::new(),
            error_message: String::new(),
            max_unit_text_bytes,
            max_total_text_bytes,
            completion_digest: None,
            completion_lease_token_hash: None,
        };
        next.jobs.insert(job_id.to_string(), job.clone());
        self.commit_state(next, &format!("job-enqueue:{job_id}"))?;
        Ok((job, false))
    }

    pub(crate) fn claim_job(
        &mut self,
        requested_job_id: Option<&str>,
        claim_request_id: &str,
        worker_id: &str,
        lease_ttl_ms: u32,
        now_ms: u64,
        is_visible: impl Fn(&GraphExtractionJob) -> bool,
    ) -> Result<Option<GraphJobClaim>> {
        validate_job_id(claim_request_id)?;
        validate_job_id(worker_id)?;
        validate_job_lease(lease_ttl_ms)?;
        if let Some(job_id) = requested_job_id {
            validate_job_id(job_id)?;
        }
        let mut next = self.state.clone();
        let recovered = recover_expired_jobs(&mut next, now_ms);
        if let Some(job) = next.jobs.values().find(|job| {
            job.claim_request_id.as_deref() == Some(claim_request_id)
                && job.worker_id.as_deref() == Some(worker_id)
                && job.state.is_active()
                && is_visible(job)
        }) {
            let replayed = job.clone();
            let lease_token = job
                .lease_token
                .clone()
                .ok_or_else(|| anyhow!("active graph job lease token is missing"))?;
            if recovered {
                self.commit_state(next, &format!("job-recover:{claim_request_id}"))?;
            }
            return Ok(Some(GraphJobClaim {
                job: replayed,
                lease_token,
            }));
        }
        let selected = if let Some(job_id) = requested_job_id {
            next.jobs
                .get(job_id)
                .filter(|job| claimable_job(job, now_ms) && is_visible(job))
                .map(|job| job.job_id.clone())
        } else {
            next.jobs
                .values()
                .filter(|job| claimable_job(job, now_ms) && is_visible(job))
                .min_by_key(|job| (job.created_at_ms, job.job_id.as_str()))
                .map(|job| job.job_id.clone())
        };
        let Some(job_id) = selected else {
            if recovered {
                self.commit_state(next, &format!("job-recover:{claim_request_id}"))?;
            }
            return Ok(None);
        };
        let generation = self.state.generation.saturating_add(1);
        let job = next
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| anyhow!("graph job disappeared during claim"))?;
        job.attempt_count = job.attempt_count.saturating_add(1);
        let lease_token = hash_hex(
            format!(
                "{}\0{}\0{}\0{}\0{}",
                job.job_id, claim_request_id, worker_id, job.attempt_count, generation
            )
            .as_bytes(),
        );
        job.state = GraphJobState::Claimed;
        job.updated_at_ms = now_ms;
        job.lease_expires_at_ms = Some(now_ms.saturating_add(u64::from(lease_ttl_ms)));
        job.lease_token = Some(lease_token.clone());
        job.claim_request_id = Some(claim_request_id.to_string());
        job.worker_id = Some(worker_id.to_string());
        job.next_attempt_at_ms = None;
        job.error_code.clear();
        job.error_message.clear();
        let claimed = job.clone();
        self.commit_state(next, &format!("job-claim:{job_id}:{claim_request_id}"))?;
        Ok(Some(GraphJobClaim {
            job: claimed,
            lease_token,
        }))
    }

    pub(crate) fn renew_job(
        &mut self,
        job_id: &str,
        lease_token: &str,
        lease_ttl_ms: u32,
        now_ms: u64,
    ) -> Result<GraphExtractionJob> {
        validate_job_id(job_id)?;
        validate_job_token(lease_token)?;
        validate_job_lease(lease_ttl_ms)?;
        let mut next = self.state.clone();
        recover_expired_jobs(&mut next, now_ms);
        let job = next
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow!("graph job not found: {job_id}"))?;
        ensure!(job.state.is_active(), "graph job lease is not active");
        ensure!(
            job.lease_token.as_deref() == Some(lease_token),
            "graph job lease token is stale"
        );
        ensure!(
            job.lease_expires_at_ms
                .is_some_and(|expiry| expiry > now_ms),
            "graph job lease expired"
        );
        if job.state == GraphJobState::Claimed {
            job.state = GraphJobState::Running;
        }
        job.updated_at_ms = now_ms;
        let renewed_expiry = now_ms.saturating_add(u64::from(lease_ttl_ms));
        job.lease_expires_at_ms = Some(
            job.lease_expires_at_ms
                .unwrap_or_default()
                .max(renewed_expiry),
        );
        let renewed = job.clone();
        self.commit_state(next, &format!("job-renew:{job_id}"))?;
        Ok(renewed)
    }

    pub(crate) fn recover_job_leases(&mut self, now_ms: u64) -> Result<bool> {
        let mut next = self.state.clone();
        if !recover_expired_jobs(&mut next, now_ms) {
            return Ok(false);
        }
        self.commit_state(next, &format!("job-recover:{now_ms}"))?;
        Ok(true)
    }

    pub(crate) fn cancel_job(
        &mut self,
        job_id: &str,
        reason: &str,
        now_ms: u64,
    ) -> Result<(GraphExtractionJob, String)> {
        validate_job_id(job_id)?;
        validate_job_error(reason, true)?;
        let mut next = self.state.clone();
        recover_expired_jobs(&mut next, now_ms);
        let job = next
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow!("graph job not found: {job_id}"))?;
        let outcome = if job.state.is_terminal() {
            "already_terminal"
        } else if job.state.is_active() {
            job.cancel_requested = true;
            job.updated_at_ms = now_ms;
            if !reason.is_empty() {
                job.error_code = "cancel_requested".to_string();
                job.error_message = reason.to_string();
            }
            "cancel_requested"
        } else {
            job.state = GraphJobState::Cancelled;
            job.cancel_requested = true;
            job.updated_at_ms = now_ms;
            clear_job_lease(job);
            job.next_attempt_at_ms = None;
            job.error_code = "cancelled".to_string();
            job.error_message = reason.to_string();
            "cancelled"
        }
        .to_string();
        let cancelled = job.clone();
        self.commit_state(next, &format!("job-cancel:{job_id}"))?;
        Ok((cancelled, outcome))
    }

    pub(crate) fn mark_job_source_changed(
        &mut self,
        job_id: &str,
        message: &str,
        now_ms: u64,
    ) -> Result<GraphExtractionJob> {
        validate_job_id(job_id)?;
        validate_job_error(message, false)?;
        let mut next = self.state.clone();
        let job = next
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow!("graph job not found: {job_id}"))?;
        if !job.state.is_terminal() {
            job.state = GraphJobState::Failed;
            job.updated_at_ms = now_ms;
            job.error_code = "source_changed".to_string();
            job.error_message = message.to_string();
            job.cancel_requested = false;
            job.next_attempt_at_ms = None;
            clear_job_lease(job);
        }
        let failed = job.clone();
        self.commit_state(next, &format!("job-source-changed:{job_id}"))?;
        Ok(failed)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_job(
        &mut self,
        job_id: &str,
        lease_token: &str,
        extraction_run_id: &str,
        outcome: GraphJobFinishOutcome,
        sources: &[GraphSource],
        entities: &[GraphEntityInput],
        relations: &[GraphRelationInput],
        error_code: &str,
        error_message: &str,
        now_ms: u64,
    ) -> Result<GraphJobFinishResult> {
        validate_job_id(job_id)?;
        validate_job_token(lease_token)?;
        validate_run_id(extraction_run_id)?;
        validate_job_error(error_code, true)?;
        validate_job_error(error_message, true)?;
        let mut next = self.state.clone();
        recover_expired_jobs(&mut next, now_ms);
        let current = next
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| anyhow!("graph job not found: {job_id}"))?;
        if current.state == GraphJobState::Completed {
            ensure!(
                current.extraction_run_id == extraction_run_id,
                "graph job completion run ID changed"
            );
            ensure!(
                outcome == GraphJobFinishOutcome::Completed,
                "completed graph job outcome changed"
            );
            let digest = upsert_digest(
                extraction_run_id,
                sources,
                &current.provider,
                entities,
                relations,
            )?;
            ensure!(
                current.completion_digest.as_deref() == Some(digest.as_str()),
                "completed graph job material changed"
            );
            let lease_token_hash = hash_hex(lease_token.as_bytes());
            ensure!(
                current.completion_lease_token_hash.as_deref() == Some(lease_token_hash.as_str()),
                "completed graph job lease token is stale"
            );
            let run = next
                .runs
                .get(extraction_run_id)
                .cloned()
                .ok_or_else(|| anyhow!("completed graph job receipt is missing"))?;
            return Ok(GraphJobFinishResult {
                job: current,
                upsert: Some(GraphUpsertOutcome {
                    run,
                    entities: Vec::new(),
                    relations: Vec::new(),
                    rejected: Vec::new(),
                    conflicts: Vec::new(),
                    warnings: vec!["identical graph job completion already committed".to_string()],
                }),
                warnings: vec!["graph job completion was already durable".to_string()],
            });
        }
        ensure!(current.state.is_active(), "graph job lease is not active");
        ensure!(
            current.lease_token.as_deref() == Some(lease_token),
            "graph job lease token is stale"
        );
        ensure!(
            current
                .lease_expires_at_ms
                .is_some_and(|expiry| expiry > now_ms),
            "graph job lease expired"
        );
        ensure!(
            current.extraction_run_id == extraction_run_id,
            "graph job completion run ID changed"
        );
        if current.cancel_requested {
            let job = next
                .jobs
                .get_mut(job_id)
                .ok_or_else(|| anyhow!("graph job disappeared during cancellation"))?;
            job.state = GraphJobState::Cancelled;
            job.updated_at_ms = now_ms;
            job.next_attempt_at_ms = None;
            job.error_code = "cancelled".to_string();
            if job.error_message.is_empty() {
                job.error_message = "graph extraction was cancelled".to_string();
            }
            clear_job_lease(job);
            let cancelled = job.clone();
            self.commit_state(next, &format!("job-finish-cancelled:{job_id}"))?;
            return Ok(GraphJobFinishResult {
                job: cancelled,
                upsert: None,
                warnings: vec!["graph job was cancelled before completion".to_string()],
            });
        }

        let mut upsert = None;
        let provider = current.provider.clone();
        match outcome {
            GraphJobFinishOutcome::Completed => {
                ensure!(
                    error_code.is_empty(),
                    "completed graph job cannot include an error"
                );
            }
            GraphJobFinishOutcome::RetryableFailure => {
                ensure!(
                    !error_code.trim().is_empty(),
                    "retryable graph job failure needs an error code"
                );
                let job = next
                    .jobs
                    .get_mut(job_id)
                    .ok_or_else(|| anyhow!("graph job disappeared during finish"))?;
                queue_or_fail_job(job, error_code, error_message, now_ms);
            }
            GraphJobFinishOutcome::PermanentFailure => {
                ensure!(
                    !error_code.trim().is_empty(),
                    "permanent graph job failure needs an error code"
                );
                let job = next
                    .jobs
                    .get_mut(job_id)
                    .ok_or_else(|| anyhow!("graph job disappeared during finish"))?;
                job.state = GraphJobState::Failed;
                job.updated_at_ms = now_ms;
                job.next_attempt_at_ms = None;
                job.error_code = error_code.to_string();
                job.error_message = error_message.to_string();
                clear_job_lease(job);
            }
        }
        if outcome == GraphJobFinishOutcome::Completed {
            let completion_digest =
                upsert_digest(extraction_run_id, sources, &provider, entities, relations)?;
            let applied = apply_upsert(
                &mut next,
                extraction_run_id,
                sources,
                &provider,
                entities,
                relations,
                now_ms,
            )?;
            let job = next
                .jobs
                .get_mut(job_id)
                .ok_or_else(|| anyhow!("graph job disappeared after upsert"))?;
            job.state = GraphJobState::Completed;
            job.updated_at_ms = now_ms;
            job.next_attempt_at_ms = None;
            job.error_code.clear();
            job.error_message.clear();
            job.completion_digest = Some(completion_digest);
            job.completion_lease_token_hash = Some(hash_hex(lease_token.as_bytes()));
            clear_job_lease(job);
            upsert = Some(applied);
        }
        let finished = next
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| anyhow!("graph job disappeared before commit"))?;
        self.commit_state(next, &format!("job-finish:{job_id}"))?;
        Ok(GraphJobFinishResult {
            job: finished,
            upsert,
            warnings: Vec::new(),
        })
    }

    pub(crate) fn commit_state(
        &mut self,
        mut next: GraphState,
        transaction_id: &str,
    ) -> Result<()> {
        next.generation = self.state.generation.saturating_add(1);
        validate_state(&next)?;
        let pending = PendingGraph {
            schema_version: GRAPH_SCHEMA_VERSION,
            transaction_id: transaction_id.to_string(),
            state: next.clone(),
        };
        write_json_atomic(&self.pending_path, &pending, GRAPH_PENDING_MAX_BYTES)?;
        if let Err(error) = write_json_atomic(&self.path, &next, GRAPH_STATE_MAX_BYTES) {
            return Err(error).context("cannot commit knowledge graph state");
        }
        self.state = next;
        remove_file_durable(&self.pending_path)?;
        Ok(())
    }

    pub(crate) fn erase_sources(
        &mut self,
        source_ids: &HashSet<String>,
        transaction_id: &str,
    ) -> Result<()> {
        if source_ids.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.mentions
            .retain(|mention| !source_ids.contains(&mention.evidence.source_memory_id));
        for relation in next.relations.values_mut() {
            relation
                .evidence
                .retain(|evidence| !source_ids.contains(&evidence.source_memory_id));
        }
        let mentioned_entities = next
            .mentions
            .iter()
            .map(|mention| mention.entity_id.clone())
            .collect::<HashSet<_>>();
        next.relations.retain(|_, relation| {
            !relation.evidence.is_empty()
                && mentioned_entities.contains(&relation.subject_entity_id)
                && mentioned_entities.contains(&relation.object_entity_id)
        });
        let relation_entities = next
            .relations
            .values()
            .flat_map(|relation| {
                [
                    relation.subject_entity_id.clone(),
                    relation.object_entity_id.clone(),
                ]
            })
            .collect::<HashSet<_>>();
        next.entities
            .retain(|id, _| relation_entities.contains(id) || mentioned_entities.contains(id));
        for run in next.runs.values_mut() {
            if run.source_ids.iter().any(|id| source_ids.contains(id)) {
                run.outcome = "source_deleted".to_string();
                run.terminal = true;
                run.source_ids.retain(|id| !source_ids.contains(id));
            }
        }
        for job in next.jobs.values_mut() {
            if !job.state.is_terminal()
                && job
                    .sources
                    .iter()
                    .any(|source| source_ids.contains(&source.source_memory_id))
            {
                job.state = GraphJobState::Failed;
                job.cancel_requested = false;
                job.next_attempt_at_ms = None;
                job.error_code = "source_changed".to_string();
                job.error_message =
                    "a source changed or was deleted before graph extraction completed".to_string();
                clear_job_lease(job);
            }
        }
        self.commit_state(next, transaction_id)
    }

    pub(crate) fn purge(&mut self, transaction_id: &str) -> Result<()> {
        self.commit_state(GraphState::default(), transaction_id)
    }

    pub(crate) fn upsert(
        &mut self,
        run_id: &str,
        sources: &[GraphSource],
        provider: &GraphProvider,
        entities: &[GraphEntityInput],
        relations: &[GraphRelationInput],
        now_ms: u64,
    ) -> Result<GraphUpsertOutcome> {
        ensure!(
            !self
                .state
                .jobs
                .values()
                .any(|job| job.extraction_run_id == run_id),
            "extraction run ID is reserved by a durable graph job"
        );
        let mut next = self.state.clone();
        let outcome = apply_upsert(
            &mut next, run_id, sources, provider, entities, relations, now_ms,
        )?;
        self.commit_state(next, &format!("run:{run_id}"))?;
        Ok(outcome)
    }

    pub(crate) fn search(
        &self,
        query: &str,
        max_depth: usize,
        max_fanout: usize,
        max_results: usize,
        eligible_entity_ids: &HashSet<String>,
        eligible_relation_ids: &HashSet<String>,
    ) -> Result<Vec<GraphSearchResult>> {
        ensure!(!query.trim().is_empty(), "graph search query is required");
        ensure!(
            max_depth <= MAX_GRAPH_DEPTH,
            "graph search depth exceeds limit"
        );
        ensure!(
            max_fanout <= MAX_GRAPH_FANOUT,
            "graph search fanout exceeds limit"
        );
        ensure!(
            (1..=MAX_GRAPH_RESULTS).contains(&max_results),
            "graph search result limit is invalid"
        );
        let query = normalize_name(query);
        let query_tokens = query.split_whitespace().collect::<Vec<_>>();
        let mut entity_scores = HashMap::<String, f64>::new();
        for (id, entity) in &self.state.entities {
            if !eligible_entity_ids.contains(id) {
                continue;
            }
            let score = lexical_score(
                &query,
                &query_tokens,
                std::iter::once(entity.normalized_name.as_str())
                    .chain(std::iter::once(entity.entity_type.as_str()))
                    .chain(entity.aliases.iter().map(String::as_str)),
            );
            if score > 0.0 {
                entity_scores.insert(id.clone(), score);
            }
        }
        for (id, relation) in &self.state.relations {
            if !eligible_relation_ids.contains(id) {
                continue;
            }
            let score = lexical_score(
                &query,
                &query_tokens,
                std::iter::once(relation.predicate.as_str())
                    .chain(std::iter::once(relation.relation_type.as_str()))
                    .chain(
                        relation
                            .evidence
                            .iter()
                            .map(|evidence| evidence.quote.as_str()),
                    ),
            );
            if score == 0.0 {
                continue;
            }
            for entity_id in [&relation.subject_entity_id, &relation.object_entity_id] {
                if eligible_entity_ids.contains(entity_id) {
                    entity_scores
                        .entry(entity_id.clone())
                        .and_modify(|current| *current = current.max(score))
                        .or_insert(score);
                }
            }
        }
        let mut queue = entity_scores
            .keys()
            .cloned()
            .map(|id| (id, 0_usize))
            .collect::<VecDeque<_>>();
        let mut visited = entity_scores.keys().cloned().collect::<HashSet<_>>();
        while let Some((entity_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let neighbors = self
                .state
                .relations
                .iter()
                .filter_map(|(relation_id, relation)| {
                    if !eligible_relation_ids.contains(relation_id) {
                        return None;
                    }
                    let neighbor = if relation.subject_entity_id == entity_id {
                        &relation.object_entity_id
                    } else if relation.object_entity_id == entity_id {
                        &relation.subject_entity_id
                    } else {
                        return None;
                    };
                    if eligible_entity_ids.contains(neighbor) {
                        Some(neighbor.clone())
                    } else {
                        None
                    }
                })
                .take(max_fanout);
            for neighbor in neighbors {
                if visited.insert(neighbor.clone()) {
                    entity_scores
                        .entry(neighbor.clone())
                        .or_insert(0.1 / (depth + 2) as f64);
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }
        let mut results = Vec::new();
        for (entity_id, score) in entity_scores {
            let Some(entity) = self.state.entities.get(&entity_id) else {
                continue;
            };
            let entity_relations = self
                .state
                .relations
                .iter()
                .filter(|(relation_id, relation)| {
                    eligible_relation_ids.contains(*relation_id)
                        && (relation.subject_entity_id == entity_id
                            || relation.object_entity_id == entity_id)
                })
                .take(max_fanout)
                .map(|(_, relation)| relation.clone())
                .collect::<Vec<_>>();
            let evidence = self
                .state
                .mentions
                .iter()
                .filter(|mention| mention.entity_id == entity_id)
                .take(MAX_GRAPH_EVIDENCE)
                .map(|mention| mention.evidence.clone())
                .collect::<Vec<_>>();
            results.push(GraphSearchResult {
                memory_id: evidence
                    .first()
                    .map(|item| item.source_memory_id.clone())
                    .unwrap_or_default(),
                score,
                entities: vec![entity.clone()],
                relations: entity_relations,
                evidence,
            });
        }
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.entities[0].entity_id.cmp(&right.entities[0].entity_id))
        });
        results.truncate(max_results);
        Ok(results)
    }

    pub(crate) fn scopes_for_source_ids(&self, source_ids: &HashSet<String>) -> Vec<GraphScope> {
        let mut scopes = Vec::new();
        for mention in &self.state.mentions {
            if source_ids.contains(&mention.evidence.source_memory_id) {
                scopes.push(mention.evidence.scope.clone());
            }
        }
        for relation in self.state.relations.values() {
            if relation
                .evidence
                .iter()
                .any(|evidence| source_ids.contains(&evidence.source_memory_id))
            {
                scopes.push(relation.scope.clone());
            }
        }
        unique_scopes_from_vec(scopes)
    }
}

fn apply_upsert(
    state: &mut GraphState,
    run_id: &str,
    sources: &[GraphSource],
    provider: &GraphProvider,
    entities: &[GraphEntityInput],
    relations: &[GraphRelationInput],
    now_ms: u64,
) -> Result<GraphUpsertOutcome> {
    validate_run_id(run_id)?;
    validate_provider(provider)?;
    ensure!(
        sources.len() <= MAX_GRAPH_UNITS,
        "graph source count exceeds limit"
    );
    ensure!(
        entities.len() <= MAX_GRAPH_ENTITIES,
        "graph entity count exceeds limit"
    );
    ensure!(
        relations.len() <= MAX_GRAPH_RELATIONS,
        "graph relation count exceeds limit"
    );
    let digest = upsert_digest(run_id, sources, provider, entities, relations)?;
    if let Some(existing) = state.runs.get(run_id) {
        if existing.idempotency_digest == digest {
            return Ok(GraphUpsertOutcome {
                run: existing.clone(),
                entities: Vec::new(),
                relations: Vec::new(),
                rejected: Vec::new(),
                conflicts: Vec::new(),
                warnings: vec!["identical extraction run already committed".to_string()],
            });
        }
        bail!("extraction run ID was already used with different material");
    }
    let source_by_unit = sources
        .iter()
        .map(|source| (source.source_unit_id.clone(), source))
        .collect::<HashMap<_, _>>();
    ensure!(
        source_by_unit.len() == sources.len(),
        "graph source unit IDs must be unique"
    );
    let mut accepted_entities = Vec::new();
    let mut accepted_relations = Vec::new();
    let mut rejected = Vec::new();
    let mut conflicts = Vec::new();
    let mut mentions = Vec::new();
    for (index, candidate) in entities.iter().enumerate() {
        match build_entity(state, candidate, &source_by_unit, now_ms, &mut mentions) {
            Ok(entity) => accepted_entities.push((index, entity)),
            Err(error) => rejected.push(GraphCandidateRejection {
                kind: "entity".to_string(),
                index,
                code: "invalid_candidate".to_string(),
                message: error.to_string(),
            }),
        }
    }

    for (index, candidate) in relations.iter().enumerate() {
        match build_relation(
            state,
            candidate,
            &source_by_unit,
            provider,
            now_ms,
            &mut mentions,
        ) {
            Ok(relation) => accepted_relations.push((index, relation)),
            Err(error) => {
                let existing_id = error
                    .to_string()
                    .strip_prefix("CONFLICT:")
                    .map(str::to_string);
                if let Some(existing_id) = existing_id {
                    conflicts.push(GraphCandidateConflict {
                        kind: "relation".to_string(),
                        index,
                        existing_id,
                        code: "relation_collision".to_string(),
                        message: "relation already exists with a different assertion".to_string(),
                    });
                } else {
                    rejected.push(GraphCandidateRejection {
                        kind: "relation".to_string(),
                        index,
                        code: "invalid_candidate".to_string(),
                        message: error.to_string(),
                    });
                }
            }
        }
    }
    state.mentions.extend(mentions);
    state.mentions.sort_by(|left, right| {
        (
            &left.entity_id,
            &left.evidence.source_memory_id,
            &left.evidence.quote,
        )
            .cmp(&(
                &right.entity_id,
                &right.evidence.source_memory_id,
                &right.evidence.quote,
            ))
    });
    state.mentions.dedup_by(|left, right| {
        left.entity_id == right.entity_id
            && left.evidence.source_memory_id == right.evidence.source_memory_id
            && left.evidence.source_unit_id == right.evidence.source_unit_id
            && left.evidence.quote == right.evidence.quote
            && left.evidence.occurrence_index == right.evidence.occurrence_index
    });
    let mut entity_sources = HashMap::<String, HashSet<String>>::new();
    for mention in &state.mentions {
        entity_sources
            .entry(mention.entity_id.clone())
            .or_default()
            .insert(mention.evidence.source_memory_id.clone());
    }
    for entity in state.entities.values_mut() {
        entity.source_count = entity_sources
            .get(&entity.entity_id)
            .map_or(0, |sources| sources.len() as u64);
    }
    let run = GraphRun {
        extraction_run_id: run_id.to_string(),
        idempotency_digest: digest,
        outcome: "committed".to_string(),
        committed_at_ms: now_ms,
        source_count: sources.len() as u64,
        accepted_entity_count: accepted_entities.len() as u64,
        accepted_relation_count: accepted_relations.len() as u64,
        rejected_candidate_count: rejected.len() as u64,
        conflict_count: conflicts.len() as u64,
        warning_count: 0,
        terminal: true,
        provider_id: provider.provider_id.clone(),
        model_id: provider.model_id.clone(),
        extractor_version: provider.extractor_version.clone(),
        prompt_version: provider.prompt_version.clone(),
        candidate_schema_version: provider.schema_version.clone(),
        scopes: unique_scopes(sources),
        source_ids: sources
            .iter()
            .map(|source| source.source_memory_id.clone())
            .collect(),
    };
    state.runs.insert(run_id.to_string(), run.clone());
    Ok(GraphUpsertOutcome {
        run,
        entities: accepted_entities,
        relations: accepted_relations,
        rejected,
        conflicts,
        warnings: Vec::new(),
    })
}

fn validate_state(state: &GraphState) -> Result<()> {
    ensure!(
        state.schema_version == GRAPH_SCHEMA_VERSION,
        "unsupported knowledge graph schema"
    );
    ensure!(
        state.policy_revision == GRAPH_POLICY_VERSION,
        "unsupported graph policy revision"
    );
    ensure!(
        state.normalization_version == ENTITY_NORMALIZATION_VERSION,
        "unsupported graph normalization version"
    );
    ensure!(
        state.resolution_version == ENTITY_RESOLUTION_VERSION,
        "unsupported graph entity resolution version"
    );
    ensure!(
        state.entities.len() <= 100_000,
        "knowledge graph has too many entities"
    );
    ensure!(
        state.relations.len() <= 100_000,
        "knowledge graph has too many relations"
    );
    ensure!(
        state.mentions.len() <= 500_000,
        "knowledge graph has too many mentions"
    );
    ensure!(
        state.jobs.len() <= MAX_GRAPH_JOBS,
        "knowledge graph has too many extraction jobs"
    );
    for (id, entity) in &state.entities {
        ensure!(
            id == &entity.entity_id,
            "graph entity key does not match entity ID"
        );
        ensure!(
            !entity.canonical_name.is_empty(),
            "graph entity name cannot be blank"
        );
        validate_scope(&entity.scope)?;
    }
    for (id, relation) in &state.relations {
        ensure!(
            id == &relation.relation_id,
            "graph relation key does not match relation ID"
        );
        ensure!(
            state.entities.contains_key(&relation.subject_entity_id),
            "graph relation subject is missing"
        );
        ensure!(
            state.entities.contains_key(&relation.object_entity_id),
            "graph relation object is missing"
        );
        ensure!(
            !relation.evidence.is_empty(),
            "active graph relation must have evidence"
        );
        ensure!(
            relation
                .invalid_at_ms
                .is_none_or(|invalid| relation.valid_at_ms.is_none_or(|valid| invalid >= valid)),
            "graph relation temporal range is invalid"
        );
        validate_scope(&relation.scope)?;
        ensure!(
            relation
                .evidence
                .iter()
                .all(|evidence| evidence.scope == relation.scope),
            "graph relation evidence crosses scopes"
        );
        for evidence in &relation.evidence {
            validate_persisted_evidence(evidence)?;
        }
    }
    for mention in &state.mentions {
        ensure!(
            state.entities.contains_key(&mention.entity_id),
            "graph mention entity is missing"
        );
        validate_persisted_evidence(&mention.evidence)?;
    }
    for (id, run) in &state.runs {
        ensure!(
            id == &run.extraction_run_id,
            "graph run key does not match run ID"
        );
        validate_run_id(id)?;
        ensure!(
            !run.idempotency_digest.is_empty()
                && !run.provider_id.is_empty()
                && !run.model_id.is_empty()
                && !run.extractor_version.is_empty()
                && !run.prompt_version.is_empty()
                && !run.candidate_schema_version.is_empty(),
            "graph run identity is incomplete"
        );
        for scope in &run.scopes {
            validate_scope(scope)?;
        }
    }
    for (id, job) in &state.jobs {
        ensure!(id == &job.job_id, "graph job key does not match job ID");
        validate_job_id(id)?;
        ensure!(
            !job.idempotency_digest.is_empty()
                && !job.extraction_run_id.is_empty()
                && (1..=MAX_GRAPH_JOB_ATTEMPTS).contains(&job.max_attempts),
            "graph job identity or attempt limit is invalid"
        );
        validate_run_id(&job.extraction_run_id)?;
        validate_provider(&job.provider)?;
        ensure!(
            !job.sources.is_empty() && job.sources.len() <= MAX_GRAPH_UNITS,
            "graph job source count is invalid"
        );
        ensure!(
            (1..=MAX_GRAPH_TEXT_BYTES as u32).contains(&job.max_unit_text_bytes)
                && (job.max_unit_text_bytes..=MAX_GRAPH_TOTAL_TEXT_BYTES as u32)
                    .contains(&job.max_total_text_bytes),
            "graph job text limits are invalid"
        );
        for source in &job.sources {
            ensure!(
                !source.source_memory_id.is_empty()
                    && !source.source_unit_id.is_empty()
                    && !source.content_hash.is_empty()
                    && !source.extraction_revision.is_empty()
                    && !source.origin.is_empty()
                    && source.remote_eligible,
                "graph job source binding is incomplete"
            );
            validate_scope(&source.scope)?;
            ensure!(
                source.policy_revision == GRAPH_POLICY_VERSION,
                "graph job source policy revision is unsupported"
            );
        }
        if job.state.is_active() {
            ensure!(
                job.lease_expires_at_ms.is_some()
                    && job.lease_token.is_some()
                    && job.claim_request_id.is_some()
                    && job.worker_id.is_some(),
                "active graph job lease is incomplete"
            );
            validate_job_token(job.lease_token.as_deref().unwrap_or_default())?;
            validate_job_id(job.claim_request_id.as_deref().unwrap_or_default())?;
            validate_job_id(job.worker_id.as_deref().unwrap_or_default())?;
        } else {
            ensure!(
                job.lease_expires_at_ms.is_none() && job.lease_token.is_none(),
                "inactive graph job cannot retain a lease"
            );
        }
        if job.state == GraphJobState::Completed {
            ensure!(
                job.completion_digest.is_some() && job.completion_lease_token_hash.is_some(),
                "completed graph job completion fence is incomplete"
            );
            validate_job_token(
                job.completion_lease_token_hash
                    .as_deref()
                    .unwrap_or_default(),
            )?;
        } else {
            ensure!(
                job.completion_digest.is_none() && job.completion_lease_token_hash.is_none(),
                "non-completed graph job cannot retain a completion fence"
            );
        }
        validate_job_error(&job.error_code, true)?;
        validate_job_error(&job.error_message, true)?;
    }
    Ok(())
}

fn validate_scope(scope: &GraphScope) -> Result<()> {
    ensure!(
        !scope.project_id.is_empty(),
        "graph scope project ID is required"
    );
    match scope.kind {
        GraphScopeKind::Project | GraphScopeKind::Repository => ensure!(
            scope.scope_key.is_none(),
            "project graph scope cannot have a scope key"
        ),
        GraphScopeKind::Agent | GraphScopeKind::Session => ensure!(
            scope.scope_key.as_ref().is_some_and(|key| !key.is_empty()),
            "agent or session graph scope requires a scope key"
        ),
    }
    Ok(())
}

fn validate_persisted_evidence(evidence: &GraphEvidence) -> Result<()> {
    ensure!(
        !evidence.source_memory_id.is_empty()
            && !evidence.source_unit_id.is_empty()
            && !evidence.content_hash.is_empty()
            && !evidence.extraction_revision.is_empty()
            && !evidence.quote.is_empty(),
        "graph evidence is incomplete"
    );
    ensure!(
        evidence.quote.chars().count() <= MAX_GRAPH_QUOTE_CHARS,
        "graph evidence quote is too long"
    );
    validate_scope(&evidence.scope)
}

fn validate_run_id(value: &str) -> Result<()> {
    ensure!(
        (1..=128).contains(&value.len()),
        "extraction run ID is invalid"
    );
    ensure!(
        !value.chars().any(char::is_control),
        "extraction run ID contains a control character"
    );
    Ok(())
}

fn validate_provider(provider: &GraphProvider) -> Result<()> {
    for (name, value) in [
        ("provider", &provider.provider_id),
        ("model", &provider.model_id),
        ("extractor", &provider.extractor_version),
        ("prompt", &provider.prompt_version),
        ("schema", &provider.schema_version),
    ] {
        ensure!(
            !value.trim().is_empty() && value.chars().count() <= MAX_GRAPH_STRING_CHARS,
            "graph {name} identity is invalid"
        );
    }
    if let Some(variant) = provider.variant.as_deref() {
        ensure!(
            !variant.trim().is_empty() && variant.chars().count() <= MAX_GRAPH_STRING_CHARS,
            "graph provider variant is invalid"
        );
    }
    Ok(())
}

fn validate_job_id(value: &str) -> Result<()> {
    validate_run_id(value).context("graph job ID is invalid")
}

fn validate_job_token(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "graph job lease token is invalid"
    );
    Ok(())
}

fn validate_job_lease(value: u32) -> Result<()> {
    ensure!(
        (MIN_GRAPH_JOB_LEASE_MS..=MAX_GRAPH_JOB_LEASE_MS).contains(&value),
        "graph job lease duration is invalid"
    );
    Ok(())
}

fn validate_job_error(value: &str, allow_empty: bool) -> Result<()> {
    ensure!(
        (allow_empty || !value.trim().is_empty())
            && value.chars().count() <= MAX_GRAPH_JOB_ERROR_CHARS
            && !value.chars().any(char::is_control),
        "graph job error is invalid"
    );
    Ok(())
}

fn clear_job_lease(job: &mut GraphExtractionJob) {
    job.lease_expires_at_ms = None;
    job.lease_token = None;
    job.claim_request_id = None;
    job.worker_id = None;
}

fn prune_terminal_jobs(state: &mut GraphState) {
    if state.jobs.len() < MAX_GRAPH_JOBS {
        return;
    }
    let remove_count = state.jobs.len().saturating_sub(MAX_GRAPH_JOBS) + 1;
    let mut terminal = state
        .jobs
        .values()
        .filter(|job| job.state.is_terminal())
        .map(|job| (job.updated_at_ms, job.job_id.clone()))
        .collect::<Vec<_>>();
    terminal.sort();
    for (_, job_id) in terminal.into_iter().take(remove_count) {
        state.jobs.remove(&job_id);
    }
}

fn claimable_job(job: &GraphExtractionJob, now_ms: u64) -> bool {
    job.state == GraphJobState::Queued
        && !job.cancel_requested
        && job.attempt_count < job.max_attempts
        && job.next_attempt_at_ms.is_none_or(|at| at <= now_ms)
}

fn retry_delay_ms(attempt_count: u32) -> u64 {
    let shift = attempt_count.saturating_sub(1).min(6);
    GRAPH_JOB_RETRY_BASE_MS
        .saturating_mul(1_u64 << shift)
        .min(MAX_GRAPH_JOB_RETRY_MS)
}

fn queue_or_fail_job(
    job: &mut GraphExtractionJob,
    error_code: &str,
    error_message: &str,
    now_ms: u64,
) {
    job.updated_at_ms = now_ms;
    job.error_code = error_code.to_string();
    job.error_message = error_message.to_string();
    job.cancel_requested = false;
    clear_job_lease(job);
    if job.attempt_count < job.max_attempts {
        job.state = GraphJobState::Queued;
        job.next_attempt_at_ms = Some(now_ms.saturating_add(retry_delay_ms(job.attempt_count)));
    } else {
        job.state = GraphJobState::Failed;
        job.next_attempt_at_ms = None;
    }
}

fn recover_expired_jobs(state: &mut GraphState, now_ms: u64) -> bool {
    let mut changed = false;
    for job in state.jobs.values_mut() {
        if !job.state.is_active()
            || !job
                .lease_expires_at_ms
                .is_some_and(|expires_at| expires_at <= now_ms)
        {
            continue;
        }
        changed = true;
        if job.cancel_requested {
            job.state = GraphJobState::Cancelled;
            job.updated_at_ms = now_ms;
            job.next_attempt_at_ms = None;
            job.error_code = "cancelled".to_string();
            if job.error_message.is_empty() {
                job.error_message = "graph extraction was cancelled".to_string();
            }
            clear_job_lease(job);
        } else {
            queue_or_fail_job(job, "lease_expired", "graph worker lease expired", now_ms);
        }
    }
    changed
}

fn job_digest(
    job_id: &str,
    sources: &[GraphSource],
    provider: &GraphProvider,
    max_attempts: u32,
    max_unit_text_bytes: u32,
    max_total_text_bytes: u32,
) -> Result<String> {
    let mut bindings = sources
        .iter()
        .map(|source| {
            serde_json::json!({
                "source_memory_id": source.source_memory_id,
                "source_unit_id": source.source_unit_id,
                "content_hash": source.content_hash,
                "extraction_revision": source.extraction_revision,
                "scope": source.scope,
                "origin": source.origin,
                "policy_revision": source.policy_revision,
                "remote_eligible": source.remote_eligible,
            })
        })
        .collect::<Vec<_>>();
    sort_json_values(&mut bindings)?;
    let material = serde_json::json!({
        "job_id": job_id,
        "sources": bindings,
        "provider": provider,
        "max_attempts": max_attempts,
        "max_unit_text_bytes": max_unit_text_bytes,
        "max_total_text_bytes": max_total_text_bytes,
    });
    Ok(hash_hex(serde_json::to_string(&material)?.as_bytes()))
}

fn build_entity(
    state: &mut GraphState,
    candidate: &GraphEntityInput,
    source_by_unit: &HashMap<String, &GraphSource>,
    now_ms: u64,
    mentions: &mut Vec<GraphMention>,
) -> Result<GraphEntity> {
    validate_name(&candidate.mention, "entity mention")?;
    validate_name(&candidate.canonical_hint, "entity canonical hint")?;
    validate_name(&candidate.entity_type, "entity type")?;
    validate_confidence(candidate.confidence)?;
    let evidence = validate_evidence(&candidate.evidence, source_by_unit)?;
    let scope = evidence_scope(&evidence)?;
    let normalized = normalize_name(if candidate.canonical_hint.trim().is_empty() {
        &candidate.mention
    } else {
        &candidate.canonical_hint
    });
    let entity_type = normalize_name(&candidate.entity_type);
    let entity_id =
        resolve_entity_id(state, &scope, &entity_type, &normalized)?.unwrap_or_else(|| {
            format!(
                "ent_{}",
                &hash_hex(entity_key(&scope, &entity_type, &normalized).as_bytes())[..32]
            )
        });
    let entity = state
        .entities
        .entry(entity_id.clone())
        .or_insert_with(|| GraphEntity {
            entity_id: entity_id.clone(),
            canonical_name: candidate.canonical_hint.clone(),
            normalized_name: normalized.clone(),
            entity_type: entity_type.clone(),
            aliases: BTreeSet::new(),
            scope: scope.clone(),
            first_seen_at_ms: now_ms,
            last_seen_at_ms: now_ms,
            source_count: 0,
        });
    ensure!(
        entity.scope == scope && entity.entity_type == entity_type,
        "entity resolution crossed graph scope or type"
    );
    entity.last_seen_at_ms = now_ms;
    entity.aliases.extend(
        candidate
            .aliases
            .iter()
            .map(|alias| normalize_name(alias))
            .chain(std::iter::once(normalize_name(&candidate.mention)))
            .filter(|alias| !alias.is_empty()),
    );
    let source_ids = evidence
        .iter()
        .map(|evidence| evidence.source_memory_id.clone())
        .collect::<HashSet<_>>();
    entity.source_count = entity.source_count.saturating_add(source_ids.len() as u64);
    for item in evidence {
        mentions.push(GraphMention {
            entity_id: entity_id.clone(),
            evidence: item,
        });
    }
    Ok(entity.clone())
}

fn build_relation(
    state: &mut GraphState,
    candidate: &GraphRelationInput,
    source_by_unit: &HashMap<String, &GraphSource>,
    provider: &GraphProvider,
    now_ms: u64,
    _mentions: &mut Vec<GraphMention>,
) -> Result<GraphRelation> {
    validate_name(&candidate.subject_mention, "relation subject")?;
    validate_name(&candidate.object_mention, "relation object")?;
    validate_name(&candidate.predicate, "relation predicate")?;
    validate_name(&candidate.relation_type, "relation type")?;
    validate_confidence(candidate.confidence)?;
    ensure!(
        matches!(
            candidate.predicate.as_str(),
            "uses"
                | "depends_on"
                | "implements"
                | "causes"
                | "related_to"
                | "supports"
                | "contradicts"
        ),
        "unsupported graph predicate"
    );
    if let Some(invalid) = candidate.invalid_at_ms {
        ensure!(
            candidate.valid_at_ms.is_none_or(|valid| invalid >= valid),
            "relation temporal range is invalid"
        );
    }
    let evidence = validate_evidence(&candidate.evidence, source_by_unit)?;
    let scope = evidence_scope(&evidence)?;
    let subject_id =
        resolve_mention_entity(state, &scope, &candidate.subject_mention).ok_or_else(|| {
            anyhow!("relation subject entity is not present in this extraction batch")
        })?;
    let object_id = resolve_mention_entity(state, &scope, &candidate.object_mention)
        .ok_or_else(|| anyhow!("relation object entity is not present in this extraction batch"))?;
    let relation_material = format!(
        "{}\0{}\0{}\0{}\0{}",
        scope_key(&scope),
        subject_id,
        candidate.predicate,
        object_id,
        candidate.relation_type
    );
    let base_relation_id = format!("rel_{}", &hash_hex(relation_material.as_bytes())[..32]);
    let relation_id = match state.relations.get(&base_relation_id) {
        Some(existing)
            if existing.valid_at_ms != candidate.valid_at_ms
                || existing.invalid_at_ms != candidate.invalid_at_ms =>
        {
            let temporal_material = format!(
                "{relation_material}\0{:?}\0{:?}",
                candidate.valid_at_ms, candidate.invalid_at_ms
            );
            format!("rel_{}", &hash_hex(temporal_material.as_bytes())[..32])
        }
        Some(_) | None => base_relation_id,
    };
    if let Some(existing) = state.relations.get_mut(&relation_id) {
        ensure!(
            existing.valid_at_ms == candidate.valid_at_ms
                && existing.invalid_at_ms == candidate.invalid_at_ms,
            "CONFLICT:{relation_id}"
        );
        existing.confidence = existing.confidence.max(candidate.confidence);
        existing.evidence.extend(evidence);
        dedup_evidence(&mut existing.evidence);
        existing.status = relation_status(existing.invalid_at_ms, now_ms).to_string();
        return Ok(existing.clone());
    }
    let relation = GraphRelation {
        relation_id: relation_id.clone(),
        subject_entity_id: subject_id,
        predicate: candidate.predicate.clone(),
        object_entity_id: object_id,
        relation_type: candidate.relation_type.clone(),
        valid_at_ms: candidate.valid_at_ms,
        invalid_at_ms: candidate.invalid_at_ms,
        created_at_ms: now_ms,
        extracted_at_ms: now_ms,
        confidence: candidate.confidence,
        status: relation_status(candidate.invalid_at_ms, now_ms).to_string(),
        evidence,
        extractor_version: provider.extractor_version.clone(),
        scope,
    };
    state.relations.insert(relation_id, relation.clone());
    Ok(relation)
}

fn relation_status(invalid_at_ms: Option<u64>, now_ms: u64) -> &'static str {
    if invalid_at_ms.is_some_and(|invalid_at| invalid_at <= now_ms) {
        "invalidated"
    } else {
        "active"
    }
}

fn resolve_mention_entity(state: &GraphState, scope: &GraphScope, mention: &str) -> Option<String> {
    let normalized = normalize_name(mention);
    let matches = state
        .entities
        .values()
        .filter(|entity| {
            entity.scope == *scope
                && (entity.normalized_name == normalized || entity.aliases.contains(&normalized))
        })
        .map(|entity| entity.entity_id.clone())
        .take(2)
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0].clone())
}

fn resolve_entity_id(
    state: &GraphState,
    scope: &GraphScope,
    entity_type: &str,
    normalized_name: &str,
) -> Result<Option<String>> {
    let matches = state
        .entities
        .values()
        .filter(|entity| entity.scope == *scope && entity.entity_type == entity_type)
        .filter(|entity| {
            entity.normalized_name == normalized_name
                || entity.aliases.contains(normalized_name)
                || token_jaccard(&entity.normalized_name, normalized_name) >= 0.9
        })
        .map(|entity| entity.entity_id.clone())
        .take(2)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() <= 1,
        "entity resolution is ambiguous within this graph scope and type"
    );
    Ok(matches.into_iter().next())
}

fn token_jaccard(left: &str, right: &str) -> f64 {
    let left = left.split_whitespace().collect::<HashSet<_>>();
    let right = right.split_whitespace().collect::<HashSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count();
    let union = left.union(&right).count();
    intersection as f64 / union as f64
}

fn validate_evidence(
    inputs: &[GraphEvidenceInput],
    source_by_unit: &HashMap<String, &GraphSource>,
) -> Result<Vec<GraphEvidence>> {
    ensure!(
        !inputs.is_empty() && inputs.len() <= MAX_GRAPH_EVIDENCE,
        "graph candidate evidence count is invalid"
    );
    let mut output = Vec::with_capacity(inputs.len());
    for input in inputs {
        validate_name(&input.source_unit_id, "evidence source unit")?;
        ensure!(
            !input.quote.is_empty() && input.quote.chars().count() <= MAX_GRAPH_QUOTE_CHARS,
            "graph evidence quote is invalid"
        );
        let source = source_by_unit
            .get(&input.source_unit_id)
            .ok_or_else(|| anyhow!("evidence references an unknown source unit"))?;
        let start = input
            .utf8_start
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
        let end = input
            .utf8_end
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
        if let (Some(start), Some(end)) = (start, end) {
            ensure!(
                start <= end && end <= source.text.len(),
                "evidence UTF-8 offsets are invalid"
            );
            ensure!(
                &source.text.as_bytes()[start..end] == input.quote.as_bytes(),
                "evidence quote does not match its UTF-8 offsets"
            );
        } else {
            ensure!(
                source.text.contains(&input.quote),
                "evidence quote is not present in source text"
            );
        }
        output.push(GraphEvidence {
            source_memory_id: source.source_memory_id.clone(),
            source_unit_id: source.source_unit_id.clone(),
            content_hash: source.content_hash.clone(),
            extraction_revision: source.extraction_revision.clone(),
            scope: source.scope.clone(),
            quote: input.quote.clone(),
            occurrence_index: input.occurrence_index,
            utf8_start: input.utf8_start,
            utf8_end: input.utf8_end,
        });
    }
    dedup_evidence(&mut output);
    Ok(output)
}

fn evidence_scope(evidence: &[GraphEvidence]) -> Result<GraphScope> {
    let scope = evidence
        .first()
        .map(|item| item.scope.clone())
        .ok_or_else(|| anyhow!("graph candidate needs evidence"))?;
    ensure!(
        evidence.iter().all(|item| item.scope == scope),
        "graph candidate evidence crosses scopes"
    );
    Ok(scope)
}

fn unique_scopes(sources: &[GraphSource]) -> Vec<GraphScope> {
    unique_scopes_from_vec(sources.iter().map(|source| source.scope.clone()).collect())
}

fn unique_scopes_from_vec(scopes: Vec<GraphScope>) -> Vec<GraphScope> {
    let mut out = Vec::new();
    for scope in scopes {
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    out
}

fn dedup_evidence(evidence: &mut Vec<GraphEvidence>) {
    evidence.sort_by(|left, right| {
        (
            &left.source_memory_id,
            &left.source_unit_id,
            &left.quote,
            left.occurrence_index,
        )
            .cmp(&(
                &right.source_memory_id,
                &right.source_unit_id,
                &right.quote,
                right.occurrence_index,
            ))
    });
    evidence.dedup_by(|left, right| {
        left.source_memory_id == right.source_memory_id
            && left.source_unit_id == right.source_unit_id
            && left.quote == right.quote
            && left.occurrence_index == right.occurrence_index
    });
    evidence.truncate(MAX_GRAPH_EVIDENCE);
}

fn validate_name(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.chars().count() <= MAX_GRAPH_STRING_CHARS,
        "{label} is invalid"
    );
    ensure!(
        !value.chars().any(char::is_control),
        "{label} contains a control character"
    );
    Ok(())
}

fn validate_confidence(value: f64) -> Result<()> {
    ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "graph confidence must be in [0, 1]"
    );
    Ok(())
}

fn normalize_name(value: &str) -> String {
    value
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn scope_key(scope: &GraphScope) -> String {
    format!(
        "{}\0{}\0{}",
        scope.project_id,
        scope.kind.as_str(),
        scope.scope_key.as_deref().unwrap_or_default()
    )
}

fn entity_key(scope: &GraphScope, entity_type: &str, name: &str) -> String {
    format!("{}\0{}\0{}", scope_key(scope), entity_type, name)
}

fn lexical_score<'a>(
    query: &str,
    tokens: &[&str],
    fields: impl IntoIterator<Item = &'a str>,
) -> f64 {
    fields
        .into_iter()
        .map(|field| lexical_field_score(query, tokens, &normalize_name(field)))
        .fold(0.0, f64::max)
}

fn lexical_field_score(query: &str, tokens: &[&str], field: &str) -> f64 {
    if field == query {
        return 1.0;
    }
    if field.contains(query) {
        return 0.75;
    }
    let matched = tokens
        .iter()
        .filter(|token| field.split_whitespace().any(|word| word == **token))
        .count();
    if matched == 0 {
        0.0
    } else {
        matched as f64 / tokens.len() as f64 * 0.6
    }
}

fn upsert_digest(
    run_id: &str,
    sources: &[GraphSource],
    provider: &GraphProvider,
    entities: &[GraphEntityInput],
    relations: &[GraphRelationInput],
) -> Result<String> {
    let mut source_bindings = sources
        .iter()
        .map(|source| {
            serde_json::json!({
                "source_memory_id": source.source_memory_id,
                "source_unit_id": source.source_unit_id,
                "content_hash": source.content_hash,
                "extraction_revision": source.extraction_revision,
                "scope": source.scope,
                "origin": source.origin,
                "policy_revision": source.policy_revision,
                "remote_eligible": source.remote_eligible,
            })
        })
        .collect::<Vec<_>>();
    sort_json_values(&mut source_bindings)?;
    let mut entity_values = entities
        .iter()
        .map(|entity| {
            let mut aliases = entity
                .aliases
                .iter()
                .map(|alias| normalize_name(alias))
                .collect::<Vec<_>>();
            aliases.sort();
            aliases.dedup();
            Ok(serde_json::json!({
                "mention": normalize_name(&entity.mention),
                "canonical_hint": normalize_name(&entity.canonical_hint),
                "entity_type": normalize_name(&entity.entity_type),
                "aliases": aliases,
                "evidence": normalized_evidence_values(&entity.evidence)?,
                "confidence": entity.confidence,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    sort_json_values(&mut entity_values)?;
    let mut relation_values = relations
        .iter()
        .map(|relation| {
            Ok(serde_json::json!({
                "subject_mention": normalize_name(&relation.subject_mention),
                "predicate": normalize_name(&relation.predicate),
                "object_mention": normalize_name(&relation.object_mention),
                "relation_type": normalize_name(&relation.relation_type),
                "valid_at_ms": relation.valid_at_ms,
                "invalid_at_ms": relation.invalid_at_ms,
                "evidence": normalized_evidence_values(&relation.evidence)?,
                "confidence": relation.confidence,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    sort_json_values(&mut relation_values)?;
    let material = serde_json::json!({
        "run": run_id,
        "sources": source_bindings,
        "provider": provider,
        "entities": entity_values,
        "relations": relation_values,
        "normalization": ENTITY_NORMALIZATION_VERSION,
        "resolution": ENTITY_RESOLUTION_VERSION,
        "policy": GRAPH_POLICY_VERSION,
    });
    Ok(hash_hex(serde_json::to_string(&material)?.as_bytes()))
}

fn normalized_evidence_values(evidence: &[GraphEvidenceInput]) -> Result<Vec<serde_json::Value>> {
    let mut values = evidence
        .iter()
        .map(|item| {
            serde_json::json!({
                "source_unit_id": item.source_unit_id,
                "quote": item.quote,
                "utf8_start": item.utf8_start,
                "utf8_end": item.utf8_end,
                "occurrence_index": item.occurrence_index,
            })
        })
        .collect::<Vec<_>>();
    sort_json_values(&mut values)?;
    Ok(values)
}

fn sort_json_values(values: &mut [serde_json::Value]) -> Result<()> {
    let mut keyed = values
        .iter()
        .cloned()
        .map(|value| Ok((serde_json::to_string(&value)?, value)))
        .collect::<Result<Vec<_>>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    for (slot, (_, value)) in values.iter_mut().zip(keyed) {
        *slot = value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> GraphSource {
        GraphSource {
            source_memory_id: "mem-1".to_string(),
            source_unit_id: "unit-1".to_string(),
            content_hash: "a".repeat(64),
            extraction_revision: "r1".to_string(),
            scope: GraphScope {
                project_id: "project".to_string(),
                kind: GraphScopeKind::Project,
                scope_key: None,
            },
            origin: "manual".to_string(),
            policy_revision: GRAPH_POLICY_VERSION.to_string(),
            remote_eligible: true,
            text: "Native memory uses Zvec.".to_string(),
        }
    }

    fn entity(mention: &str) -> GraphEntityInput {
        GraphEntityInput {
            mention: mention.to_string(),
            canonical_hint: mention.to_string(),
            entity_type: "technology".to_string(),
            aliases: Vec::new(),
            evidence: vec![GraphEvidenceInput {
                source_unit_id: "unit-1".to_string(),
                quote: mention.to_string(),
                utf8_start: None,
                utf8_end: None,
                occurrence_index: 0,
            }],
            confidence: 0.9,
        }
    }

    fn provider() -> GraphProvider {
        GraphProvider {
            provider_id: "provider".to_string(),
            model_id: "model".to_string(),
            extractor_version: "extractor-v1".to_string(),
            prompt_version: "prompt-v1".to_string(),
            schema_version: "schema-v1".to_string(),
            variant: None,
        }
    }

    fn graph_store(temp: &tempfile::TempDir) -> GraphStore {
        GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load graph")
    }

    fn enqueue_job(store: &mut GraphStore, job_id: &str, max_attempts: u32) -> GraphExtractionJob {
        store
            .enqueue_job(
                job_id,
                &[source()],
                &provider(),
                max_attempts,
                MAX_GRAPH_TEXT_BYTES as u32,
                MAX_GRAPH_TOTAL_TEXT_BYTES as u32,
                1,
            )
            .expect("enqueue")
            .0
    }

    #[test]
    fn durable_jobs_survive_restart_and_enqueue_is_idempotent() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        let first = enqueue_job(&mut store, "job-restart", 3);
        drop(store);

        let mut restarted = graph_store(&temp);
        assert_eq!(
            restarted.job("job-restart").map(|job| job.state),
            Some(GraphJobState::Queued)
        );
        let (replayed, existing) = restarted
            .enqueue_job(
                "job-restart",
                &[source()],
                &provider(),
                3,
                MAX_GRAPH_TEXT_BYTES as u32,
                MAX_GRAPH_TOTAL_TEXT_BYTES as u32,
                2,
            )
            .expect("replay enqueue");
        assert!(existing);
        assert_eq!(replayed.idempotency_digest, first.idempotency_digest);

        let mut changed_provider = provider();
        changed_provider.model_id = "other-model".to_string();
        assert!(
            restarted
                .enqueue_job(
                    "job-restart",
                    &[source()],
                    &changed_provider,
                    3,
                    MAX_GRAPH_TEXT_BYTES as u32,
                    MAX_GRAPH_TOTAL_TEXT_BYTES as u32,
                    3,
                )
                .is_err()
        );
    }

    #[test]
    fn current_graph_state_without_jobs_migrates_additively() {
        let temp = tempfile::tempdir().expect("temp");
        let graph = temp.path().join("graph.json");
        let mut legacy = serde_json::to_value(GraphState::default()).expect("encode state");
        legacy.as_object_mut().expect("state object").remove("jobs");
        std::fs::write(
            &graph,
            serde_json::to_vec_pretty(&legacy).expect("encode legacy"),
        )
        .expect("write legacy state");

        let loaded = GraphStore::load(&graph, &temp.path().join("graph.pending.json"))
            .expect("load current graph state without jobs");

        assert!(loaded.state.jobs.is_empty());
    }

    #[test]
    fn expired_leases_retry_until_the_attempt_bound() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        enqueue_job(&mut store, "job-lease", 2);
        let first = store
            .claim_job(
                Some("job-lease"),
                "claim-1",
                "worker-1",
                MIN_GRAPH_JOB_LEASE_MS,
                10,
                |_| true,
            )
            .expect("claim")
            .expect("claimed");
        let first_expiry = first.job.lease_expires_at_ms.expect("expiry");

        store
            .recover_job_leases(first_expiry)
            .expect("recover expired lease");
        let queued = store.job("job-lease").expect("queued job");
        assert_eq!(queued.state, GraphJobState::Queued);
        assert_eq!(queued.attempt_count, 1);
        let retry_at = queued.next_attempt_at_ms.expect("retry time");
        assert!(
            store
                .claim_job(
                    Some("job-lease"),
                    "claim-too-early",
                    "worker-2",
                    MIN_GRAPH_JOB_LEASE_MS,
                    retry_at - 1,
                    |_| true,
                )
                .expect("early claim")
                .is_none()
        );
        let second = store
            .claim_job(
                Some("job-lease"),
                "claim-2",
                "worker-2",
                MIN_GRAPH_JOB_LEASE_MS,
                retry_at,
                |_| true,
            )
            .expect("second claim")
            .expect("reclaimed");
        assert_ne!(second.lease_token, first.lease_token);
        store
            .recover_job_leases(second.job.lease_expires_at_ms.expect("second expiry"))
            .expect("recover final lease");
        let failed = store.job("job-lease").expect("failed job");
        assert_eq!(failed.state, GraphJobState::Failed);
        assert_eq!(failed.error_code, "lease_expired");
    }

    #[test]
    fn restart_recovers_an_expired_claim_without_losing_the_job() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        enqueue_job(&mut store, "job-restart-lease", 2);
        store
            .claim_job(
                Some("job-restart-lease"),
                "claim-before-restart",
                "worker-before-restart",
                MIN_GRAPH_JOB_LEASE_MS,
                1,
                |_| true,
            )
            .expect("claim")
            .expect("claimed");
        drop(store);

        let restarted = graph_store(&temp);
        let recovered = restarted.job("job-restart-lease").expect("recovered job");
        assert_eq!(recovered.state, GraphJobState::Queued);
        assert_eq!(recovered.attempt_count, 1);
        assert_eq!(recovered.error_code, "lease_expired");
        assert!(recovered.lease_token.is_none());
    }

    #[test]
    fn source_mutation_fences_an_active_job() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        let job = enqueue_job(&mut store, "job-source-change", 3);
        let claim = store
            .claim_job(
                Some(&job.job_id),
                "claim-source-change",
                "worker-source-change",
                MIN_GRAPH_JOB_LEASE_MS,
                10,
                |_| true,
            )
            .expect("claim")
            .expect("claimed");
        store
            .erase_sources(&HashSet::from(["mem-1".to_string()]), "source-change")
            .expect("invalidate source");

        let failed = store.job(&job.job_id).expect("failed job");
        assert_eq!(failed.state, GraphJobState::Failed);
        assert_eq!(failed.error_code, "source_changed");
        assert!(
            store
                .finish_job(
                    &job.job_id,
                    &claim.lease_token,
                    &job.extraction_run_id,
                    GraphJobFinishOutcome::Completed,
                    &[source()],
                    &[entity("Zvec")],
                    &[],
                    "",
                    "",
                    20,
                )
                .is_err()
        );
        assert!(store.run(&job.extraction_run_id).is_none());
    }

    #[test]
    fn queued_job_cancellation_is_idempotent() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        enqueue_job(&mut store, "job-cancel", 3);

        let (cancelled, outcome) = store
            .cancel_job("job-cancel", "user cancelled", 10)
            .expect("cancel");
        assert_eq!(outcome, "cancelled");
        assert_eq!(cancelled.state, GraphJobState::Cancelled);
        let (replayed, replay_outcome) = store
            .cancel_job("job-cancel", "user cancelled", 11)
            .expect("replay cancel");
        assert_eq!(replay_outcome, "already_terminal");
        assert_eq!(replayed.state, GraphJobState::Cancelled);
    }

    #[test]
    fn job_completion_commits_receipt_and_state_atomically_and_idempotently() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        let job = enqueue_job(&mut store, "job-complete", 3);
        assert!(
            store
                .upsert(
                    &job.extraction_run_id,
                    &[source()],
                    &provider(),
                    &[entity("Zvec")],
                    &[],
                    9,
                )
                .is_err()
        );
        let claim = store
            .claim_job(
                Some(&job.job_id),
                "claim-complete",
                "worker-complete",
                MIN_GRAPH_JOB_LEASE_MS,
                10,
                |_| true,
            )
            .expect("claim")
            .expect("claimed");
        store
            .renew_job(&job.job_id, &claim.lease_token, MIN_GRAPH_JOB_LEASE_MS, 11)
            .expect("mark running");
        assert_eq!(
            store.job(&job.job_id).map(|job| job.state),
            Some(GraphJobState::Running)
        );
        let completed = store
            .finish_job(
                &job.job_id,
                &claim.lease_token,
                &job.extraction_run_id,
                GraphJobFinishOutcome::Completed,
                &[source()],
                &[entity("Zvec")],
                &[],
                "",
                "",
                12,
            )
            .expect("complete");
        assert_eq!(completed.job.state, GraphJobState::Completed);
        assert!(completed.upsert.is_some());
        drop(store);

        let mut restarted = graph_store(&temp);
        assert_eq!(
            restarted.job(&job.job_id).map(|job| job.state),
            Some(GraphJobState::Completed)
        );
        assert!(restarted.run(&job.extraction_run_id).is_some());
        let replayed = restarted
            .finish_job(
                &job.job_id,
                &claim.lease_token,
                &job.extraction_run_id,
                GraphJobFinishOutcome::Completed,
                &[source()],
                &[entity("Zvec")],
                &[],
                "",
                "",
                13,
            )
            .expect("replay complete");
        assert_eq!(replayed.job.state, GraphJobState::Completed);
        assert_eq!(restarted.state.runs.len(), 1);
        assert!(
            restarted
                .finish_job(
                    &job.job_id,
                    &claim.lease_token,
                    &job.extraction_run_id,
                    GraphJobFinishOutcome::Completed,
                    &[source()],
                    &[entity("Native")],
                    &[],
                    "",
                    "",
                    14,
                )
                .is_err()
        );
        assert!(
            restarted
                .finish_job(
                    &job.job_id,
                    &"b".repeat(64),
                    &job.extraction_run_id,
                    GraphJobFinishOutcome::Completed,
                    &[source()],
                    &[entity("Zvec")],
                    &[],
                    "",
                    "",
                    15,
                )
                .is_err()
        );
    }

    #[test]
    fn normalizes_unicode_and_scope_before_resolving_entities() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let first = store
            .upsert(
                "run-1",
                &[source()],
                &GraphProvider {
                    provider_id: "p".to_string(),
                    model_id: "m".to_string(),
                    extractor_version: "v".to_string(),
                    prompt_version: "p".to_string(),
                    schema_version: "s".to_string(),
                    variant: None,
                },
                &[entity("Zvec")],
                &[],
                1,
            )
            .expect("upsert");
        assert_eq!(first.entities.len(), 1);
        let mut second_source = source();
        second_source.source_memory_id = "mem-2".to_string();
        second_source.text = "zvec".to_string();
        let second = store
            .upsert(
                "run-2",
                &[second_source],
                &GraphProvider {
                    provider_id: "p".to_string(),
                    model_id: "m".to_string(),
                    extractor_version: "v".to_string(),
                    prompt_version: "p".to_string(),
                    schema_version: "s".to_string(),
                    variant: None,
                },
                &[entity("zvec")],
                &[],
                2,
            )
            .expect("upsert");
        assert_eq!(
            second.entities[0].1.entity_id,
            first.entities[0].1.entity_id
        );
    }

    #[test]
    fn identical_run_is_idempotent_and_changed_material_is_rejected() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let provider = GraphProvider {
            provider_id: "p".to_string(),
            model_id: "m".to_string(),
            extractor_version: "v".to_string(),
            prompt_version: "p".to_string(),
            schema_version: "s".to_string(),
            variant: None,
        };
        let first = store
            .upsert("run-1", &[source()], &provider, &[entity("zvec")], &[], 1)
            .expect("upsert");
        let replay = store
            .upsert("run-1", &[source()], &provider, &[entity("zvec")], &[], 2)
            .expect("replay");
        assert_eq!(replay.run.idempotency_digest, first.run.idempotency_digest);
        assert!(
            store
                .upsert("run-1", &[source()], &provider, &[entity("other")], &[], 3)
                .is_err()
        );
        let ordered = store
            .upsert(
                "run-order",
                &[source()],
                &provider,
                &[entity("Native"), entity("Zvec")],
                &[],
                4,
            )
            .expect("ordered candidates");
        let reordered = store
            .upsert(
                "run-order",
                &[source()],
                &provider,
                &[entity("Zvec"), entity("Native")],
                &[],
                5,
            )
            .expect("reordered candidate replay");
        assert_eq!(
            reordered.run.idempotency_digest,
            ordered.run.idempotency_digest
        );
    }

    #[test]
    fn source_erasure_removes_evidence_and_orphan_entities() {
        let temp = tempfile::tempdir().expect("temp");
        let graph = temp.path().join("graph.json");
        let pending = temp.path().join("graph.pending.json");
        let mut store = GraphStore::load(&graph, &pending).expect("load");
        let provider = GraphProvider {
            provider_id: "p".to_string(),
            model_id: "m".to_string(),
            extractor_version: "v".to_string(),
            prompt_version: "p".to_string(),
            schema_version: "s".to_string(),
            variant: None,
        };
        let source = source();
        store
            .upsert(
                "run-1",
                std::slice::from_ref(&source),
                &provider,
                &[entity("zvec")],
                &[],
                1,
            )
            .expect("upsert");
        store
            .erase_sources(&HashSet::from(["mem-1".to_string()]), "delete:mem-1")
            .expect("erase");
        assert!(store.state.entities.is_empty());
        assert!(store.state.mentions.is_empty());
        assert_eq!(store.state.runs["run-1"].outcome, "source_deleted");
        assert!(graph.exists());
        let mut replay_source = source;
        replay_source.text.clear();
        let replay = store
            .upsert(
                "run-1",
                &[replay_source],
                &provider,
                &[entity("zvec")],
                &[],
                2,
            )
            .expect("reconcile deleted-source replay");
        assert_eq!(replay.run.outcome, "source_deleted");
        assert!(store.state.entities.is_empty());
    }

    #[test]
    fn relations_require_same_scope_source_evidence() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let mut source = source();
        source.text = "native memory uses zvec".to_string();
        let relation = GraphRelationInput {
            subject_mention: "native memory".to_string(),
            predicate: "uses".to_string(),
            object_mention: "zvec".to_string(),
            relation_type: "technology_dependency".to_string(),
            valid_at_ms: None,
            invalid_at_ms: None,
            evidence: vec![GraphEvidenceInput {
                source_unit_id: "unit-1".to_string(),
                quote: "uses".to_string(),
                utf8_start: None,
                utf8_end: None,
                occurrence_index: 0,
            }],
            confidence: 0.9,
        };

        let result = store
            .upsert(
                "run-relation",
                &[source],
                &provider(),
                &[entity("native memory"), entity("zvec")],
                &[relation],
                1,
            )
            .expect("upsert relation");

        assert_eq!(result.relations.len(), 1);
        assert_eq!(store.state.relations.len(), 1);
    }

    #[test]
    fn temporal_relation_versions_preserve_invalidation_history() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = graph_store(&temp);
        let mut graph_source = source();
        graph_source.text = "Native memory uses Zvec.".to_string();
        let first = GraphRelationInput {
            subject_mention: "Native memory".to_string(),
            predicate: "uses".to_string(),
            object_mention: "Zvec".to_string(),
            relation_type: "technology_dependency".to_string(),
            valid_at_ms: Some(10),
            invalid_at_ms: Some(20),
            evidence: vec![GraphEvidenceInput {
                source_unit_id: "unit-1".to_string(),
                quote: "uses".to_string(),
                utf8_start: None,
                utf8_end: None,
                occurrence_index: 0,
            }],
            confidence: 0.9,
        };
        let mut current = first.clone();
        current.valid_at_ms = Some(20);
        current.invalid_at_ms = None;

        let first_outcome = store
            .upsert(
                "run-temporal-1",
                std::slice::from_ref(&graph_source),
                &provider(),
                &[entity("Native memory"), entity("Zvec")],
                &[first],
                30,
            )
            .expect("store invalidated assertion");
        let current_outcome = store
            .upsert(
                "run-temporal-2",
                &[graph_source],
                &provider(),
                &[entity("Native memory"), entity("Zvec")],
                &[current],
                30,
            )
            .expect("store current assertion");

        assert_eq!(store.state.relations.len(), 2);
        assert_ne!(
            first_outcome.relations[0].1.relation_id,
            current_outcome.relations[0].1.relation_id
        );
        assert_eq!(first_outcome.relations[0].1.status, "invalidated");
        assert_eq!(current_outcome.relations[0].1.status, "active");
    }

    #[test]
    fn recovery_installs_a_pending_graph_transaction() {
        let temp = tempfile::tempdir().expect("temp");
        let graph = temp.path().join("graph.json");
        let pending_path = temp.path().join("graph.pending.json");
        let mut state = GraphState {
            generation: 1,
            ..GraphState::default()
        };
        state.runs.insert(
            "run-recovery".to_string(),
            GraphRun {
                extraction_run_id: "run-recovery".to_string(),
                idempotency_digest: "digest".to_string(),
                outcome: "committed".to_string(),
                committed_at_ms: 1,
                source_count: 0,
                accepted_entity_count: 0,
                accepted_relation_count: 0,
                rejected_candidate_count: 0,
                conflict_count: 0,
                warning_count: 0,
                terminal: true,
                provider_id: "provider".to_string(),
                model_id: "model".to_string(),
                extractor_version: "extractor-v1".to_string(),
                prompt_version: "prompt-v1".to_string(),
                candidate_schema_version: "schema-v1".to_string(),
                scopes: Vec::new(),
                source_ids: BTreeSet::new(),
            },
        );
        let pending = PendingGraph {
            schema_version: GRAPH_SCHEMA_VERSION,
            transaction_id: "recover".to_string(),
            state,
        };
        std::fs::write(
            &pending_path,
            serde_json::to_vec_pretty(&pending).expect("encode pending"),
        )
        .expect("write pending");

        let recovered = GraphStore::load(&graph, &pending_path).expect("recover pending graph");

        assert!(recovered.run("run-recovery").is_some());
        assert!(graph.exists());
        assert!(!pending_path.exists());
    }

    #[test]
    fn ambiguous_entity_types_do_not_resolve_relations_arbitrarily() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let mut source = source();
        source.text = "backend related to backend".to_string();
        let first = entity("backend");
        let mut second = entity("backend");
        second.entity_type = "concept".to_string();
        let relation = GraphRelationInput {
            subject_mention: "backend".to_string(),
            predicate: "related_to".to_string(),
            object_mention: "backend".to_string(),
            relation_type: "association".to_string(),
            valid_at_ms: None,
            invalid_at_ms: None,
            evidence: vec![GraphEvidenceInput {
                source_unit_id: "unit-1".to_string(),
                quote: "related to".to_string(),
                utf8_start: None,
                utf8_end: None,
                occurrence_index: 0,
            }],
            confidence: 0.8,
        };

        let result = store
            .upsert(
                "run-ambiguous",
                &[source],
                &provider(),
                &[first, second],
                &[relation],
                1,
            )
            .expect("persist unambiguous candidates only");

        assert!(result.relations.is_empty());
        assert_eq!(result.rejected.len(), 1);
        assert!(store.state.relations.is_empty());
    }

    #[test]
    fn bounded_token_matching_resolves_only_within_scope_and_type() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let mut first_source = source();
        first_source.text = "Native Memory".to_string();
        let first = store
            .upsert(
                "run-fuzzy-1",
                &[first_source],
                &provider(),
                &[entity("Native Memory")],
                &[],
                1,
            )
            .expect("first entity");
        let mut second_source = source();
        second_source.source_memory_id = "mem-2".to_string();
        second_source.text = "Memory Native".to_string();
        let second = store
            .upsert(
                "run-fuzzy-2",
                &[second_source],
                &provider(),
                &[entity("Memory Native")],
                &[],
                2,
            )
            .expect("token-order match");

        assert_eq!(
            first.entities[0].1.entity_id,
            second.entities[0].1.entity_id
        );
    }

    #[test]
    fn relation_predicate_alias_and_evidence_seed_search() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let mut graph_source = source();
        graph_source.text = "native memory uses zvec as a critical connector".to_string();
        let mut native = entity("native memory");
        native.aliases = vec!["project memory".to_string()];
        let relation = GraphRelationInput {
            subject_mention: "native memory".to_string(),
            predicate: "uses".to_string(),
            object_mention: "zvec".to_string(),
            relation_type: "technology_dependency".to_string(),
            valid_at_ms: None,
            invalid_at_ms: None,
            evidence: vec![GraphEvidenceInput {
                source_unit_id: "unit-1".to_string(),
                quote: "critical connector".to_string(),
                utf8_start: None,
                utf8_end: None,
                occurrence_index: 0,
            }],
            confidence: 0.9,
        };
        store
            .upsert(
                "run-search-seeds",
                &[graph_source],
                &provider(),
                &[native, entity("zvec")],
                &[relation],
                1,
            )
            .expect("upsert search graph");
        let eligible_entities = store.state.entities.keys().cloned().collect::<HashSet<_>>();
        let eligible_relations = store
            .state
            .relations
            .keys()
            .cloned()
            .collect::<HashSet<_>>();

        for query in ["project memory", "uses", "critical connector"] {
            let results = store
                .search(
                    query,
                    0,
                    MAX_GRAPH_FANOUT,
                    MAX_GRAPH_RESULTS,
                    &eligible_entities,
                    &eligible_relations,
                )
                .expect("search");
            assert!(!results.is_empty(), "query {query:?} did not seed search");
        }
        let predicate_results = store
            .search(
                "uses",
                0,
                MAX_GRAPH_FANOUT,
                MAX_GRAPH_RESULTS,
                &eligible_entities,
                &eligible_relations,
            )
            .expect("predicate search");
        assert!(
            predicate_results
                .iter()
                .flat_map(|result| &result.relations)
                .any(|result| result.predicate == "uses")
        );
    }

    #[test]
    fn ineligible_relation_cannot_consume_fanout_or_bridge_bfs() {
        let temp = tempfile::tempdir().expect("temp");
        let mut store = GraphStore::load(
            &temp.path().join("graph.json"),
            &temp.path().join("graph.pending.json"),
        )
        .expect("load");
        let scope = source().scope;
        let graph_evidence = GraphEvidence {
            source_memory_id: "mem-1".to_string(),
            source_unit_id: "unit-1".to_string(),
            content_hash: "a".repeat(64),
            extraction_revision: "r1".to_string(),
            scope: scope.clone(),
            quote: "connector".to_string(),
            occurrence_index: 0,
            utf8_start: None,
            utf8_end: None,
        };
        for (id, name) in [
            ("a", "alpha"),
            ("b", "stale bridge"),
            ("c", "behind bridge"),
            ("d", "eligible neighbor"),
        ] {
            store.state.entities.insert(
                id.to_string(),
                GraphEntity {
                    entity_id: id.to_string(),
                    canonical_name: name.to_string(),
                    normalized_name: name.to_string(),
                    entity_type: "concept".to_string(),
                    aliases: BTreeSet::new(),
                    scope: scope.clone(),
                    first_seen_at_ms: 1,
                    last_seen_at_ms: 1,
                    source_count: 1,
                },
            );
        }
        for (id, subject, object) in [
            ("rel-0-stale", "a", "b"),
            ("rel-1-current", "a", "d"),
            ("rel-2-behind-stale", "b", "c"),
        ] {
            store.state.relations.insert(
                id.to_string(),
                GraphRelation {
                    relation_id: id.to_string(),
                    subject_entity_id: subject.to_string(),
                    predicate: "related_to".to_string(),
                    object_entity_id: object.to_string(),
                    relation_type: "association".to_string(),
                    valid_at_ms: None,
                    invalid_at_ms: None,
                    created_at_ms: 1,
                    extracted_at_ms: 1,
                    confidence: 1.0,
                    status: "active".to_string(),
                    evidence: vec![graph_evidence.clone()],
                    extractor_version: "extractor-v1".to_string(),
                    scope: scope.clone(),
                },
            );
        }
        let eligible_entities = store.state.entities.keys().cloned().collect::<HashSet<_>>();
        let eligible_relations = HashSet::from([
            "rel-1-current".to_string(),
            "rel-2-behind-stale".to_string(),
        ]);

        let results = store
            .search(
                "alpha",
                2,
                1,
                MAX_GRAPH_RESULTS,
                &eligible_entities,
                &eligible_relations,
            )
            .expect("filtered graph search");
        let entity_ids = results
            .iter()
            .flat_map(|result| &result.entities)
            .map(|entity| entity.entity_id.as_str())
            .collect::<HashSet<_>>();
        let relation_ids = results
            .iter()
            .flat_map(|result| &result.relations)
            .map(|relation| relation.relation_id.as_str())
            .collect::<HashSet<_>>();

        assert_eq!(entity_ids, HashSet::from(["a", "d"]));
        assert_eq!(relation_ids, HashSet::from(["rel-1-current"]));
    }
}
