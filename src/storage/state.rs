use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::config::hash_hex;
use crate::contract::{
    CodeAnchor, DeleteReason, FeedbackEvent, FeedbackStats, MemoryKind, MemoryOrigin, MemoryScope,
};
use crate::taxonomy::MemoryTaxonomy;

pub(crate) const STATE_SCHEMA_VERSION: u32 = 4;
const RETRIEVAL_RETENTION_MS: i64 = 30 * 86_400_000;
const MAX_RETRIEVALS: usize = 1_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryMetadata {
    pub scope: MemoryScope,
    #[serde(default)]
    pub scope_key: Option<String>,
    pub origin: MemoryOrigin,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    pub half_life_days: f32,
    #[serde(default)]
    pub code_anchors: Vec<CodeAnchor>,
    #[serde(default)]
    pub feedback: FeedbackStats,
    #[serde(default)]
    pub shared_source: Option<String>,
    pub pinned: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    /// Phase 1 taxonomy. Required on v3 records.
    pub taxonomy: MemoryTaxonomy,
    /// Phase 1 confidence in `[0, 1]`. Required on v3 records.
    pub confidence: f32,
    /// Active successor that supersedes this memory, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Direct predecessors this memory supersedes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    /// Memories that conflict with this memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_with: Vec<String>,
}

impl MemoryMetadata {
    /// Whether this record was superseded by an active successor.
    #[must_use]
    pub(crate) fn is_superseded(&self) -> bool {
        self.superseded_by.is_some()
    }

    fn validate(&self, id: &str) -> Result<()> {
        ensure!(
            self.locked || self.lock_reason.is_none(),
            "unlocked memory cannot retain a lock_reason"
        );
        ensure!(
            self.lock_reason
                .as_ref()
                .is_none_or(|reason| reason.chars().count() <= 240 && !reason.contains('\0')),
            "memory lock_reason is invalid"
        );
        ensure!(
            self.scope != MemoryScope::Repository || (!self.pinned && !self.locked),
            "repository memory cannot be pinned or locked"
        );
        ensure!(
            self.confidence.is_finite(),
            "memory {id} confidence must be finite"
        );
        ensure!(
            (0.0..=1.0).contains(&self.confidence),
            "memory {id} confidence must be in [0, 1]"
        );
        ensure!(
            self.superseded_by.as_deref() != Some(id),
            "memory {id} cannot supersede itself"
        );
        ensure!(
            !self.supersedes.iter().any(|sid| sid == id),
            "memory {id} cannot supersede itself"
        );
        ensure!(
            !self.conflict_with.iter().any(|cid| cid == id),
            "memory {id} cannot conflict with itself"
        );
        ensure!(
            !self
                .superseded_by
                .as_ref()
                .is_some_and(|sid| sid.trim().is_empty()),
            "memory {id} superseded_by cannot be blank"
        );
        ensure!(
            self.supersedes.iter().all(|sid| !sid.trim().is_empty()),
            "memory {id} supersedes cannot contain blank ids"
        );
        ensure!(
            self.conflict_with.iter().all(|cid| !cid.trim().is_empty()),
            "memory {id} conflict_with cannot contain blank ids"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tombstone {
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
pub(crate) struct RetrievalRecord {
    pub query_hash: String,
    pub memory_ids: Vec<String>,
    pub created_at_ms: i64,
    #[serde(default)]
    pub events: Vec<FeedbackEvent>,
    #[serde(default)]
    pub event_memory_ids: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingDocument {
    pub id: String,
    pub title: String,
    pub content: String,
    pub search_text: String,
    pub kind: MemoryKind,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source: String,
    pub content_hash: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub embedding: Vec<f32>,
}

impl PendingDocument {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.trim().is_empty(),
            "pending upsert id cannot be blank"
        );
        ensure!(
            self.importance.is_finite() && (0.0..=1.0).contains(&self.importance),
            "pending upsert importance must be in [0, 1]"
        );
        ensure!(
            self.created_at_ms >= 0 && self.updated_at_ms >= self.created_at_ms,
            "pending upsert timestamps are invalid"
        );
        ensure!(
            !self.embedding.is_empty() && self.embedding.iter().all(|value| value.is_finite()),
            "pending upsert embedding is invalid"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingUpsert {
    pub document: PendingDocument,
    pub metadata: MemoryMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predecessor_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revive_fingerprint: Option<String>,
}

impl PendingUpsert {
    fn validate(&self, key: &str) -> Result<()> {
        self.document.validate()?;
        ensure!(
            self.document.id == key,
            "pending upsert key does not match document id"
        );
        ensure!(
            !self.predecessor_ids.iter().any(|id| id == key),
            "pending upsert cannot supersede itself"
        );
        self.metadata.validate(key)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryState {
    pub schema_version: u32,
    pub generation: u64,
    #[serde(default)]
    pub records: HashMap<String, MemoryMetadata>,
    #[serde(default)]
    pub tombstones: HashMap<String, Tombstone>,
    #[serde(default)]
    pub retrievals: HashMap<String, RetrievalRecord>,
    #[serde(default)]
    pub pending_deletes: HashSet<String>,
    #[serde(default)]
    pub pending_upserts: HashMap<String, PendingUpsert>,
    /// Authoritative semantic/lifecycle revision timestamps by memory ID.
    #[serde(default)]
    pub record_revisions: HashMap<String, i64>,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            generation: 0,
            records: HashMap::new(),
            tombstones: HashMap::new(),
            retrievals: HashMap::new(),
            pending_deletes: HashSet::new(),
            pending_upserts: HashMap::new(),
            record_revisions: HashMap::new(),
        }
    }
}

impl MemoryState {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)
            .with_context(|| format!("cannot read memory state {}", path.display()))?;
        let header: SchemaHeader = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid memory state {}", path.display()))?;

        ensure!(
            header.schema_version == STATE_SCHEMA_VERSION,
            "unsupported memory state schema {}; expected {STATE_SCHEMA_VERSION}",
            header.schema_version
        );
        let state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid memory state {}", path.display()))?;
        state.validate()?;
        Ok(state)
    }

    pub(crate) fn save(&mut self, path: &Path) -> Result<()> {
        self.normalize_relations();
        self.validate()?;
        let previous_generation = self.generation;
        self.generation = self.generation.saturating_add(1);
        if let Err(error) = self.write_without_generation_change(path) {
            self.generation = previous_generation;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn metadata(&self, id: &str) -> Result<MemoryMetadata> {
        self.records.get(id).cloned().ok_or_else(|| {
            anyhow::anyhow!("memory {id} has no lifecycle metadata; run memory_doctor")
        })
    }

    pub(crate) fn record_revision(&self, id: &str, fallback: i64) -> i64 {
        self.record_revisions.get(id).copied().unwrap_or(fallback)
    }

    pub(crate) fn set_record_revision(&mut self, id: impl Into<String>, updated_at_ms: i64) {
        self.record_revisions.insert(id.into(), updated_at_ms);
    }

    pub(crate) fn is_tombstoned(&self, fingerprint: &str) -> bool {
        self.tombstones.contains_key(fingerprint)
    }

    pub(crate) fn add_tombstone(&mut self, tombstone: Tombstone) {
        self.tombstones
            .insert(tombstone.fingerprint.clone(), tombstone);
    }

    pub(crate) fn prune_retrievals(&mut self, now_ms: i64) -> usize {
        let before = self.retrievals.len();
        self.retrievals.retain(|_, record| {
            now_ms.saturating_sub(record.created_at_ms) <= RETRIEVAL_RETENTION_MS
        });
        if self.retrievals.len() > MAX_RETRIEVALS {
            let mut oldest = self
                .retrievals
                .iter()
                .map(|(id, record)| (id.clone(), record.created_at_ms))
                .collect::<Vec<_>>();
            oldest.sort_by_key(|(_, created)| *created);
            let remove_count = oldest.len().saturating_sub(MAX_RETRIEVALS);
            for (id, _) in oldest.into_iter().take(remove_count) {
                self.retrievals.remove(&id);
            }
        }
        before.saturating_sub(self.retrievals.len())
    }

    fn normalize_relations(&mut self) {
        for metadata in self.records.values_mut() {
            if let Some(sid) = metadata.superseded_by.take() {
                let trimmed = sid.trim();
                metadata.superseded_by = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            metadata.supersedes = normalize_id_list(metadata.supersedes.clone());
            metadata.conflict_with = normalize_id_list(metadata.conflict_with.clone());
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == STATE_SCHEMA_VERSION,
            "memory state schema must be {STATE_SCHEMA_VERSION}"
        );
        for (id, metadata) in &self.records {
            metadata.validate(id)?;
        }
        for (id, pending) in &self.pending_upserts {
            pending.validate(id)?;
        }
        for (id, updated_at_ms) in &self.record_revisions {
            ensure!(
                self.records.contains_key(id),
                "record revision references missing memory {id}"
            );
            ensure!(
                *updated_at_ms >= 0,
                "record revision for {id} cannot be negative"
            );
        }
        self.validate_relations()?;
        Ok(())
    }

    fn validate_relations(&self) -> Result<()> {
        for (id, metadata) in &self.records {
            for sid in &metadata.supersedes {
                ensure!(
                    self.records.contains_key(sid),
                    "memory {id} supersedes dangling id {sid}"
                );
            }
            if let Some(sid) = &metadata.superseded_by {
                ensure!(
                    self.records.contains_key(sid),
                    "memory {id} superseded_by dangling id {sid}"
                );
            }
            for cid in &metadata.conflict_with {
                ensure!(
                    self.records.contains_key(cid),
                    "memory {id} conflicts with dangling id {cid}"
                );
            }
            for sid in &metadata.supersedes {
                let other = &self.records[sid];
                ensure!(
                    other.superseded_by.as_deref() == Some(id.as_str()),
                    "memory {id} supersedes {sid} but {sid}.superseded_by is not {id}"
                );
            }
            if let Some(sid) = &metadata.superseded_by {
                let other = &self.records[sid];
                ensure!(
                    other.supersedes.iter().any(|entry| entry == id),
                    "memory {id} superseded_by {sid} but {sid}.supersedes does not contain {id}"
                );
            }
            for cid in &metadata.conflict_with {
                let other = &self.records[cid];
                ensure!(
                    other.conflict_with.iter().any(|entry| entry == id),
                    "memory {id} conflicts with {cid} but {cid}.conflict_with does not contain {id}"
                );
            }
        }
        self.detect_supersession_cycles()?;
        Ok(())
    }

    fn detect_supersession_cycles(&self) -> Result<()> {
        for start in self.records.keys() {
            let mut current = start.clone();
            for _ in 0..self.records.len() {
                let Some(metadata) = self.records.get(&current) else {
                    break;
                };
                match &metadata.superseded_by {
                    Some(next) if next == start => {
                        bail!("supersession cycle detected involving {start}")
                    }
                    Some(next) => current = next.clone(),
                    None => break,
                }
            }
        }
        Ok(())
    }

    fn write_without_generation_change(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        if temporary.exists() {
            fs::remove_file(&temporary)
                .with_context(|| format!("cannot remove stale {}", temporary.display()))?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        set_private_file_permissions(&file)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)
            .with_context(|| format!("cannot install memory state at {}", path.display()))?;
        sync_parent(path)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

fn normalize_id_list(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = ids
        .into_iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert(id.clone()))
        .collect();
    out.sort();
    out
}

pub(crate) fn memory_fingerprint(
    kind: MemoryKind,
    scope: MemoryScope,
    scope_key: Option<&str>,
    content: &str,
) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    hash_hex(
        format!(
            "{}\0{}\0{}\0{}",
            kind.as_str(),
            scope.as_str(),
            scope_key.unwrap_or_default(),
            normalized
        )
        .as_bytes(),
    )
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn set_private_file_permissions(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MemoryMetadata, MemoryState, memory_fingerprint};
    use crate::contract::{FeedbackStats, MemoryKind, MemoryOrigin, MemoryScope};
    use crate::lifecycle::default_expiry;
    use crate::taxonomy::MemoryTaxonomy;
    use std::fs;

    const ID: &str = "mem_00000000000000000000000000000000";

    #[test]
    fn rejects_future_schema_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let future = br#"{"schema_version":99,"generation":3,"records":"not-a-map"}"#;
        fs::write(&path, future).expect("write future state");

        let error = MemoryState::load(&path).expect_err("reject future state");

        assert!(
            error
                .to_string()
                .contains("unsupported memory state schema")
        );
        assert_eq!(fs::read(&path).expect("read unchanged state"), future);
    }

    #[test]
    fn rejects_non_current_schema_without_migration() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        fs::write(&path, br#"{"schema_version":3}"#).expect("write old state");

        let error = MemoryState::load(&path).expect_err("reject old state");

        assert!(error.to_string().contains("expected 4"));
    }

    #[test]
    fn failed_save_restores_generation() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let mut state = MemoryState {
            generation: 7,
            ..MemoryState::default()
        };

        let error = state
            .save(&temp.path().join("missing").join("state.json"))
            .expect_err("save must fail");

        assert!(error.to_string().contains("cannot create"));
        assert_eq!(state.generation, 7);
    }

    #[test]
    fn current_round_trip_preserves_lifecycle_and_taxonomy_metadata() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let mut state = MemoryState::default();
        state.records.insert(
            ID.to_string(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: Some(123),
                half_life_days: 42.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: true,
                locked: true,
                lock_reason: Some("curated decision".to_string()),
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.8,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: Vec::new(),
            },
        );

        state.save(&path).expect("save state");
        let loaded = MemoryState::load(&path).expect("load state");
        let metadata = loaded.records.get(ID).expect("metadata");

        assert_eq!(loaded.generation, 1);
        assert!(metadata.pinned);
        assert!(metadata.locked);
        assert_eq!(metadata.lock_reason.as_deref(), Some("curated decision"));
        assert_eq!(metadata.taxonomy, MemoryTaxonomy::Decision);
        assert!((metadata.confidence - 0.8).abs() < f32::EPSILON);
        assert_eq!(metadata.superseded_by, None);
        assert!(metadata.supersedes.is_empty());
        assert!(metadata.conflict_with.is_empty());
    }

    #[test]
    fn fingerprint_includes_scope_and_normalizes_whitespace() {
        let first = memory_fingerprint(
            MemoryKind::Fact,
            MemoryScope::Project,
            None,
            "Use  Rust\nfor memory",
        );
        let second = memory_fingerprint(
            MemoryKind::Fact,
            MemoryScope::Project,
            None,
            "Use Rust for memory",
        );
        let repository = memory_fingerprint(
            MemoryKind::Fact,
            MemoryScope::Repository,
            None,
            "Use Rust for memory",
        );
        assert_eq!(first, second);
        assert_ne!(first, repository);
    }

    #[test]
    fn durable_decisions_do_not_expire_by_default() {
        assert_eq!(default_expiry(MemoryKind::Decision, 10), None);
        assert!(default_expiry(MemoryKind::Summary, 10).is_some());
    }

    fn other_id() -> &'static str {
        "mem_11111111111111111111111111111111"
    }

    fn state_with_relations() -> MemoryState {
        let mut state = MemoryState::default();
        let a = ID.to_string();
        let b = other_id().to_string();
        state.records.insert(
            a.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.7,
                superseded_by: Some(b.clone()),
                supersedes: Vec::new(),
                conflict_with: Vec::new(),
            },
        );
        state.records.insert(
            b.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.6,
                superseded_by: None,
                supersedes: vec![a.clone()],
                conflict_with: Vec::new(),
            },
        );
        state
    }

    #[test]
    fn validates_reciprocal_supersession_links() {
        let mut state = state_with_relations();
        // Break reciprocity: set A.superseded_by = B but remove A from B.supersedes.
        let b = other_id().to_string();
        if let Some(meta) = state.records.get_mut(&b) {
            meta.supersedes.clear();
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_dangling_superseded_by() {
        let mut state = state_with_relations();
        let dangling = "mem_ffffffffffffffffffffffffffffffff".to_string();
        if let Some(meta) = state.records.get_mut(ID) {
            meta.superseded_by = Some(dangling);
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_self_supersession() {
        let mut state = state_with_relations();
        if let Some(meta) = state.records.get_mut(ID) {
            meta.superseded_by = Some(ID.to_string());
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_supersession_cycle() {
        let mut state = MemoryState::default();
        let a = ID.to_string();
        let b = other_id().to_string();
        state.records.insert(
            a.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: Some(b.clone()),
                supersedes: vec![b.clone()],
                conflict_with: Vec::new(),
            },
        );
        state.records.insert(
            b.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: Some(a.clone()),
                supersedes: vec![a.clone()],
                conflict_with: Vec::new(),
            },
        );
        assert!(state.validate().is_err());
    }

    #[test]
    fn validates_symmetric_conflict_links() {
        let mut state = MemoryState::default();
        let a = ID.to_string();
        let b = other_id().to_string();
        state.records.insert(
            a.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: vec![b.clone()],
            },
        );
        state.records.insert(
            b.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: Vec::new(), // Missing A — asymmetric!
            },
        );
        assert!(state.validate().is_err());
    }

    #[test]
    fn normalize_relations_dedupes_and_sorts() {
        let mut state = MemoryState::default();
        let a = ID.to_string();
        let b = other_id().to_string();
        state.records.insert(
            a.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: None,
                supersedes: vec![b.clone(), b.clone(), " ".to_string()],
                conflict_with: vec![b.clone(), b.clone()],
            },
        );
        state.records.insert(
            b.clone(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 0.5,
                superseded_by: Some(a.clone()),
                supersedes: Vec::new(),
                conflict_with: vec![a.clone()],
            },
        );
        state.normalize_relations();
        let meta = &state.records[&a];
        assert_eq!(meta.supersedes, vec![b.clone()]);
        assert_eq!(meta.conflict_with, vec![b.clone()]);
    }

    #[test]
    fn rejects_non_finite_confidence() {
        let mut state = MemoryState::default();
        state.records.insert(
            ID.to_string(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: f32::NAN,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: Vec::new(),
            },
        );
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_out_of_range_confidence() {
        let mut state = MemoryState::default();
        state.records.insert(
            ID.to_string(),
            MemoryMetadata {
                scope: MemoryScope::Project,
                scope_key: None,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                half_life_days: 365.0,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                shared_source: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                taxonomy: MemoryTaxonomy::Decision,
                confidence: 1.5,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: Vec::new(),
            },
        );
        assert!(state.validate().is_err());
    }

    #[test]
    fn chain_resolution_follows_superseded_by() {
        let mut state = MemoryState::default();
        let a = ID.to_string();
        let b = other_id().to_string();
        let c = "mem_22222222222222222222222222222222".to_string();
        state.records.insert(
            a.clone(),
            MemoryMetadata {
                superseded_by: Some(b.clone()),
                supersedes: Vec::new(),
                ..metadata_with_superseded_by(None)
            },
        );
        state.records.insert(
            b.clone(),
            MemoryMetadata {
                superseded_by: Some(c.clone()),
                supersedes: vec![a.clone()],
                ..metadata_with_superseded_by(None)
            },
        );
        state.records.insert(
            c.clone(),
            MemoryMetadata {
                superseded_by: None,
                supersedes: vec![b.clone()],
                ..metadata_with_superseded_by(None)
            },
        );
        assert!(state.validate().is_ok());
        let mut current = a.clone();
        for _ in 0..10 {
            match state
                .records
                .get(&current)
                .and_then(|m| m.superseded_by.clone())
            {
                Some(next) => current = next,
                None => break,
            }
        }
        assert_eq!(current, c);
    }

    fn metadata_with_superseded_by(superseded_by: Option<String>) -> MemoryMetadata {
        MemoryMetadata {
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_at_ms: None,
            half_life_days: 365.0,
            code_anchors: Vec::new(),
            feedback: FeedbackStats::default(),
            shared_source: None,
            pinned: false,
            locked: false,
            lock_reason: None,
            taxonomy: MemoryTaxonomy::Decision,
            confidence: 0.5,
            superseded_by,
            supersedes: Vec::new(),
            conflict_with: Vec::new(),
        }
    }
}
