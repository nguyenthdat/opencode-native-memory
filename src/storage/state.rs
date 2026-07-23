use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::config::hash_hex;
use crate::contract::{
    CodeAnchor, DeleteReason, FeedbackEvent, FeedbackStats, MemoryKind, MemoryOrigin, MemoryScope,
};
use crate::lifecycle::{default_expiry, default_half_life_days};
use crate::taxonomy::MemoryTaxonomy;

pub(crate) const STATE_SCHEMA_VERSION: u32 = 3;
const LEGACY_STATE_SCHEMA_VERSION: u32 = 1;
const LEGACY_STATE_SCHEMA_VERSION_2: u32 = 2;
const RETRIEVAL_RETENTION_MS: i64 = 30 * 86_400_000;
const MAX_RETRIEVALS: usize = 1_000;
const MAX_BACKUP_TEMP_ATTEMPTS: usize = 128;
const MIGRATION_DEFAULT_CONFIDENCE: f32 = 0.5;

static BACKUP_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    pub(crate) fn legacy(kind: MemoryKind, created_at_ms: i64, importance: f32) -> Self {
        let confidence = if importance.is_finite() {
            importance.clamp(0.0, 1.0)
        } else {
            MIGRATION_DEFAULT_CONFIDENCE
        };
        Self {
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Legacy,
            expires_at_ms: default_expiry(kind, created_at_ms),
            half_life_days: default_half_life_days(kind),
            code_anchors: Vec::new(),
            feedback: FeedbackStats::default(),
            shared_source: None,
            pinned: false,
            locked: false,
            lock_reason: None,
            taxonomy: MemoryTaxonomy::infer(kind, MemoryScope::Project, &[]),
            confidence,
            superseded_by: None,
            supersedes: Vec::new(),
            conflict_with: Vec::new(),
        }
    }

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

#[derive(Debug, Deserialize, Serialize)]
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
    /// Set to `Some(1)` or `Some(2)` when a v1/v2 state was just migrated to
    /// v3 on load. The engine clears it after enriching migrated records from
    /// the zvec collection (kind/importance) on the next `open()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_migrated_from: Option<u32>,
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
            schema_migrated_from: None,
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

        match header.schema_version {
            STATE_SCHEMA_VERSION => {
                let state: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid memory state {}", path.display()))?;
                state.validate()?;
                Ok(state)
            }
            LEGACY_STATE_SCHEMA_VERSION_2 => Self::migrate_v2(path, &bytes),
            LEGACY_STATE_SCHEMA_VERSION => Self::migrate_v1(path, &bytes),
            version if version > STATE_SCHEMA_VERSION => bail!(
                "unsupported future memory state schema {version}; maximum supported version is {STATE_SCHEMA_VERSION}"
            ),
            version => bail!(
                "unsupported memory state schema {version}; expected {LEGACY_STATE_SCHEMA_VERSION}, {LEGACY_STATE_SCHEMA_VERSION_2}, or {STATE_SCHEMA_VERSION}"
            ),
        }
    }

    pub(crate) fn save(&mut self, path: &Path) -> Result<()> {
        self.normalize_relations();
        self.validate()?;
        self.generation = self.generation.saturating_add(1);
        self.write_without_generation_change(path)
    }

    pub(crate) fn metadata(
        &self,
        id: &str,
        kind: MemoryKind,
        created_at_ms: i64,
        importance: f32,
    ) -> MemoryMetadata {
        self.records
            .get(id)
            .cloned()
            .unwrap_or_else(|| MemoryMetadata::legacy(kind, created_at_ms, importance))
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

    fn migrate_v1(path: &Path, bytes: &[u8]) -> Result<Self> {
        let legacy: MemoryStateV1 = serde_json::from_slice(bytes)
            .with_context(|| format!("invalid v1 memory state {}", path.display()))?;
        let state = Self::from(legacy);
        state.validate()?;
        create_migration_backup(path, bytes, LEGACY_STATE_SCHEMA_VERSION)?;
        state.write_without_generation_change(path)?;
        Ok(state)
    }

    fn migrate_v2(path: &Path, bytes: &[u8]) -> Result<Self> {
        let legacy: MemoryStateV2 = serde_json::from_slice(bytes)
            .with_context(|| format!("invalid v2 memory state {}", path.display()))?;
        let state = Self::from(legacy);
        state.validate()?;
        create_migration_backup(path, bytes, LEGACY_STATE_SCHEMA_VERSION_2)?;
        state.write_without_generation_change(path)?;
        Ok(state)
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

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == STATE_SCHEMA_VERSION,
            "memory state schema must be {STATE_SCHEMA_VERSION}"
        );
        for (id, metadata) in &self.records {
            metadata.validate(id)?;
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryMetadataV1 {
    scope: MemoryScope,
    #[serde(default)]
    scope_key: Option<String>,
    origin: MemoryOrigin,
    #[serde(default)]
    expires_at_ms: Option<i64>,
    half_life_days: f32,
    #[serde(default)]
    code_anchors: Vec<CodeAnchor>,
    #[serde(default)]
    feedback: FeedbackStats,
    #[serde(default)]
    shared_source: Option<String>,
}

impl From<MemoryMetadataV1> for MemoryMetadata {
    fn from(value: MemoryMetadataV1) -> Self {
        Self {
            scope: value.scope,
            scope_key: value.scope_key,
            origin: value.origin,
            expires_at_ms: value.expires_at_ms,
            half_life_days: value.half_life_days,
            code_anchors: value.code_anchors.clone(),
            feedback: value.feedback,
            shared_source: value.shared_source,
            pinned: false,
            locked: false,
            lock_reason: None,
            taxonomy: MemoryTaxonomy::infer(MemoryKind::Summary, value.scope, &value.code_anchors),
            confidence: MIGRATION_DEFAULT_CONFIDENCE,
            superseded_by: None,
            supersedes: Vec::new(),
            conflict_with: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryStateV1 {
    schema_version: u32,
    generation: u64,
    #[serde(default)]
    records: HashMap<String, MemoryMetadataV1>,
    #[serde(default)]
    tombstones: HashMap<String, Tombstone>,
    #[serde(default)]
    retrievals: HashMap<String, RetrievalRecord>,
    #[serde(default)]
    pending_deletes: HashSet<String>,
}

impl From<MemoryStateV1> for MemoryState {
    fn from(value: MemoryStateV1) -> Self {
        debug_assert_eq!(value.schema_version, LEGACY_STATE_SCHEMA_VERSION);
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            generation: value.generation,
            records: value
                .records
                .into_iter()
                .map(|(id, metadata)| (id, metadata.into()))
                .collect(),
            tombstones: value.tombstones,
            retrievals: value.retrievals,
            pending_deletes: value.pending_deletes,
            schema_migrated_from: Some(LEGACY_STATE_SCHEMA_VERSION),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryMetadataV2 {
    scope: MemoryScope,
    #[serde(default)]
    scope_key: Option<String>,
    origin: MemoryOrigin,
    #[serde(default)]
    expires_at_ms: Option<i64>,
    half_life_days: f32,
    #[serde(default)]
    code_anchors: Vec<CodeAnchor>,
    #[serde(default)]
    feedback: FeedbackStats,
    #[serde(default)]
    shared_source: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    lock_reason: Option<String>,
}

impl From<MemoryMetadataV2> for MemoryMetadata {
    fn from(value: MemoryMetadataV2) -> Self {
        Self {
            scope: value.scope,
            scope_key: value.scope_key,
            origin: value.origin,
            expires_at_ms: value.expires_at_ms,
            half_life_days: value.half_life_days,
            code_anchors: value.code_anchors.clone(),
            feedback: value.feedback,
            shared_source: value.shared_source,
            pinned: value.pinned,
            locked: value.locked,
            lock_reason: value.lock_reason,
            taxonomy: MemoryTaxonomy::infer(MemoryKind::Summary, value.scope, &value.code_anchors),
            confidence: MIGRATION_DEFAULT_CONFIDENCE,
            superseded_by: None,
            supersedes: Vec::new(),
            conflict_with: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryStateV2 {
    schema_version: u32,
    generation: u64,
    #[serde(default)]
    records: HashMap<String, MemoryMetadataV2>,
    #[serde(default)]
    tombstones: HashMap<String, Tombstone>,
    #[serde(default)]
    retrievals: HashMap<String, RetrievalRecord>,
    #[serde(default)]
    pending_deletes: HashSet<String>,
}

impl From<MemoryStateV2> for MemoryState {
    fn from(value: MemoryStateV2) -> Self {
        debug_assert_eq!(value.schema_version, LEGACY_STATE_SCHEMA_VERSION_2);
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            generation: value.generation,
            records: value
                .records
                .into_iter()
                .map(|(id, metadata)| (id, metadata.into()))
                .collect(),
            tombstones: value.tombstones,
            retrievals: value.retrievals,
            pending_deletes: value.pending_deletes,
            schema_migrated_from: Some(LEGACY_STATE_SCHEMA_VERSION_2),
        }
    }
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

fn create_migration_backup(state_path: &Path, bytes: &[u8], version: u32) -> Result<()> {
    let backup_path = migration_backup_path(state_path, version);
    match fs::symlink_metadata(&backup_path) {
        Ok(_) => return verify_existing_migration_backup(&backup_path, bytes, version),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", backup_path.display()));
        }
    }

    let mut pending = create_private_backup_temp(&backup_path)?;
    pending
        .file_mut()
        .write_all(bytes)
        .and_then(|()| pending.file_mut().sync_all())
        .with_context(|| {
            format!(
                "cannot write backup temporary file {}",
                pending.path().display()
            )
        })?;
    pending.close();

    match fs::hard_link(pending.path(), &backup_path) {
        Ok(()) => {
            pending.remove().with_context(|| {
                format!(
                    "cannot remove backup temporary file {}",
                    pending.path().display()
                )
            })?;
            sync_parent(&backup_path)?;
            verify_existing_migration_backup(&backup_path, bytes, version)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            pending.remove().with_context(|| {
                format!(
                    "cannot remove backup temporary file {}",
                    pending.path().display()
                )
            })?;
            verify_existing_migration_backup(&backup_path, bytes, version)
        }
        Err(error) => Err(error)
            .with_context(|| format!("cannot install backup at {}", backup_path.display())),
    }
}

fn migration_backup_path(state_path: &Path, version: u32) -> PathBuf {
    state_path.with_file_name(format!("state.v{version}.backup.json"))
}

#[cfg(test)]
fn v1_backup_path(state_path: &Path) -> PathBuf {
    migration_backup_path(state_path, LEGACY_STATE_SCHEMA_VERSION)
}

#[cfg(test)]
fn v2_backup_path(state_path: &Path) -> PathBuf {
    migration_backup_path(state_path, LEGACY_STATE_SCHEMA_VERSION_2)
}

fn create_private_backup_temp(backup_path: &Path) -> Result<PendingBackup> {
    let parent = backup_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("backup path has no parent: {}", backup_path.display()))?;
    let base_name = backup_path.file_name().ok_or_else(|| {
        anyhow::anyhow!("backup path has no file name: {}", backup_path.display())
    })?;

    for _ in 0..MAX_BACKUP_TEMP_ATTEMPTS {
        let sequence = BACKUP_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = base_name.to_os_string();
        temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp_path) {
            Ok(file) => {
                let pending = PendingBackup::new(temp_path, file);
                set_private_file_permissions(pending.file())?;
                return Ok(pending);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "cannot create backup temporary file {}",
                        temp_path.display()
                    )
                });
            }
        }
    }
    bail!(
        "cannot allocate a unique backup temporary file beside {}",
        backup_path.display()
    )
}

fn verify_existing_migration_backup(
    backup_path: &Path,
    expected: &[u8],
    version: u32,
) -> Result<()> {
    let path_metadata = fs::symlink_metadata(backup_path)
        .with_context(|| format!("cannot inspect existing backup {}", backup_path.display()))?;
    ensure!(
        !path_metadata.file_type().is_symlink(),
        "existing v{version} backup is a symlink: {}",
        backup_path.display()
    );
    ensure!(
        path_metadata.file_type().is_file(),
        "existing v{version} backup is not a regular file: {}",
        backup_path.display()
    );
    ensure_private_backup_permissions(backup_path, &path_metadata)?;
    ensure!(
        path_metadata.len() == u64::try_from(expected.len())?,
        "existing v{version} backup does not exactly match state.json: {}",
        backup_path.display()
    );

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(backup_path)
        .with_context(|| format!("cannot open existing backup {}", backup_path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("cannot inspect open backup {}", backup_path.display()))?;
    ensure!(
        opened_metadata.is_file(),
        "existing v{version} backup is not a regular file: {}",
        backup_path.display()
    );
    ensure_private_backup_permissions(backup_path, &opened_metadata)?;
    ensure_same_file(backup_path, &path_metadata, &opened_metadata)?;

    let read_limit = u64::try_from(expected.len())?.saturating_add(1);
    let mut actual = Vec::with_capacity(expected.len());
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut actual)
        .with_context(|| format!("cannot read existing backup {}", backup_path.display()))?;
    ensure!(
        actual == expected,
        "existing v{version} backup does not exactly match state.json: {}",
        backup_path.display()
    );

    let current_metadata = fs::symlink_metadata(backup_path)
        .with_context(|| format!("cannot recheck existing backup {}", backup_path.display()))?;
    ensure!(
        !current_metadata.file_type().is_symlink() && current_metadata.file_type().is_file(),
        "existing v{version} backup changed while it was being verified: {}",
        backup_path.display()
    );
    ensure_same_file(backup_path, &opened_metadata, &current_metadata)?;
    file.sync_all()
        .with_context(|| format!("cannot fsync existing backup {}", backup_path.display()))?;
    sync_parent(backup_path)
}

#[cfg(unix)]
fn ensure_private_backup_permissions(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o7777;
    ensure!(
        mode == 0o600,
        "existing v1 backup is not private (mode {mode:o}, expected 600): {}",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_backup_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn ensure_same_file(path: &Path, left: &fs::Metadata, right: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    ensure!(
        left.dev() == right.dev() && left.ino() == right.ino(),
        "existing v1 backup changed while it was being verified: {}",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_same_file(_path: &Path, _left: &fs::Metadata, _right: &fs::Metadata) -> Result<()> {
    Ok(())
}

struct PendingBackup {
    path: PathBuf,
    file: Option<File>,
}

impl PendingBackup {
    fn new(path: PathBuf, file: File) -> Self {
        Self {
            path,
            file: Some(file),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file(&self) -> &File {
        self.file
            .as_ref()
            .expect("pending backup file must remain open")
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("pending backup file must remain open")
    }

    fn close(&mut self) {
        self.file.take();
    }

    fn remove(&mut self) -> io::Result<()> {
        self.close();
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for PendingBackup {
    fn drop(&mut self) {
        let _ = self.remove();
    }
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
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::{
        MemoryMetadata, MemoryState, STATE_SCHEMA_VERSION, memory_fingerprint, v1_backup_path,
        v2_backup_path,
    };
    use crate::contract::{FeedbackStats, MemoryKind, MemoryOrigin, MemoryScope};
    use crate::lifecycle::default_expiry;
    use crate::taxonomy::MemoryTaxonomy;

    const ID: &str = "mem_00000000000000000000000000000000";

    fn v1_state() -> String {
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "generation": 17,
            "records": {
                (ID): {
                    "scope": "project",
                    "scope_key": null,
                    "origin": "manual",
                    "expires_at_ms": 123,
                    "half_life_days": 180.0,
                    "code_anchors": [{
                        "path": "src/lib.rs",
                        "sha256": "abc",
                        "git_sha": "def"
                    }],
                    "feedback": {"injected": 1, "used": 2, "ignored": 3, "error": 4},
                    "shared_source": null
                }
            },
            "tombstones": {
                "fingerprint": {
                    "fingerprint": "fingerprint",
                    "kind": "fact",
                    "scope": "project",
                    "scope_key": null,
                    "deleted_at_ms": 99,
                    "reason": "obsolete"
                }
            },
            "retrievals": {
                "ret_000000000000000000000000": {
                    "query_hash": "query",
                    "memory_ids": [ID],
                    "created_at_ms": 88,
                    "events": ["used"],
                    "event_memory_ids": {"used": [ID]}
                }
            },
            "pending_deletes": [ID]
        }))
        .expect("serialize v1 fixture")
    }

    fn write_private_file(path: &Path, bytes: impl AsRef<[u8]>) {
        fs::write(path, bytes).expect("write private fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("set private fixture permissions");
        }
    }

    fn assert_state_unchanged(path: &Path, original: &str) {
        assert_eq!(
            fs::read(path).expect("read unchanged state"),
            original.as_bytes()
        );
    }

    #[test]
    fn migrates_v1_with_private_atomic_backup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");

        let state = MemoryState::load(&path).expect("migrate state");

        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.generation, 17);
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.tombstones.len(), 1);
        assert_eq!(state.retrievals.len(), 1);
        assert!(state.pending_deletes.contains(ID));
        let metadata = state.records.get(ID).expect("migrated metadata");
        assert_eq!(metadata.expires_at_ms, Some(123));
        assert_eq!(metadata.feedback.used, 2);
        assert_eq!(metadata.code_anchors[0].path, "src/lib.rs");
        assert!(!metadata.pinned);
        assert!(!metadata.locked);
        assert_eq!(metadata.lock_reason, None);
        let tombstone = state.tombstones.get("fingerprint").expect("tombstone");
        assert_eq!(tombstone.deleted_at_ms, 99);
        let retrieval = state
            .retrievals
            .get("ret_000000000000000000000000")
            .expect("retrieval");
        assert_eq!(retrieval.memory_ids, [ID]);
        assert_eq!(retrieval.events.len(), 1);
        let backup = v1_backup_path(&path);
        assert_eq!(fs::read_to_string(&backup).expect("read backup"), original);
        assert!(
            fs::read_dir(temp.path())
                .expect("list migration directory")
                .all(|entry| !entry
                    .expect("read migration entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(backup)
                    .expect("backup metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn accepts_preexisting_exact_private_v1_backup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");
        write_private_file(&backup, &original);

        let state = MemoryState::load(&path).expect("migrate with exact backup");

        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.generation, 17);
        assert_eq!(
            fs::read_to_string(backup).expect("read exact backup"),
            original
        );
    }

    #[test]
    fn rejects_partial_or_mismatched_existing_v1_backup_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");
        write_private_file(&backup, &original.as_bytes()[..original.len() / 2]);

        let error = MemoryState::load(&path).expect_err("reject partial backup");

        assert!(error.to_string().contains("does not exactly match"));
        assert_state_unchanged(&path, &original);
    }

    #[test]
    fn rejects_same_length_mismatched_existing_v1_backup_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let original = v1_state();
        let mut mismatched = original.as_bytes().to_vec();
        let byte = mismatched.last_mut().expect("mismatched backup byte");
        *byte = if *byte == b' ' { b'\n' } else { b' ' };
        fs::write(&path, &original).expect("write v1 state");
        write_private_file(&backup, &mismatched);

        let error = MemoryState::load(&path).expect_err("reject mismatched backup");

        assert!(error.to_string().contains("does not exactly match"));
        assert_state_unchanged(&path, &original);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_insecure_existing_v1_backup_without_modifying_state() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");
        fs::write(&backup, &original).expect("write insecure backup");
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o644))
            .expect("set insecure permissions");

        let error = MemoryState::load(&path).expect_err("reject insecure backup");

        assert!(error.to_string().contains("is not private"));
        assert_state_unchanged(&path, &original);
    }

    #[test]
    fn rejects_non_regular_existing_v1_backup_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");
        fs::create_dir(&backup).expect("create backup directory");

        let error = MemoryState::load(&path).expect_err("reject backup directory");

        assert!(error.to_string().contains("not a regular file"));
        assert_state_unchanged(&path, &original);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_existing_v1_backup_without_modifying_state() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v1_backup_path(&path);
        let target = temp.path().join("backup-target.json");
        let original = v1_state();
        fs::write(&path, &original).expect("write v1 state");
        write_private_file(&target, &original);
        symlink(&target, &backup).expect("create backup symlink");

        let error = MemoryState::load(&path).expect_err("reject backup symlink");

        assert!(error.to_string().contains("is a symlink"));
        assert_state_unchanged(&path, &original);
    }

    #[test]
    fn rejects_future_schema_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let future = br#"{"schema_version":99,"generation":3,"records":"not-a-map"}"#;
        fs::write(&path, future).expect("write future state");

        let error = MemoryState::load(&path).expect_err("reject future state");

        assert!(error.to_string().contains("future memory state schema"));
        assert_eq!(fs::read(&path).expect("read unchanged state"), future);
        assert!(!v1_backup_path(&path).exists());
    }

    #[test]
    fn v3_round_trip_preserves_lifecycle_and_taxonomy_metadata() {
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
        assert_eq!(loaded.schema_migrated_from, None);
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

    fn v2_state() -> String {
        serde_json::to_string_pretty(&json!({
            "schema_version": 2,
            "generation": 42,
            "records": {
                (ID): {
                    "scope": "project",
                    "scope_key": null,
                    "origin": "manual",
                    "expires_at_ms": 200,
                    "half_life_days": 180.0,
                    "code_anchors": [],
                    "feedback": {"injected": 0, "used": 1, "ignored": 0, "error": 0},
                    "shared_source": null,
                    "pinned": true,
                    "locked": false,
                    "lock_reason": null
                }
            },
            "tombstones": {},
            "retrievals": {},
            "pending_deletes": []
        }))
        .expect("serialize v2 fixture")
    }

    fn other_id() -> &'static str {
        "mem_11111111111111111111111111111111"
    }

    #[test]
    fn migrates_v2_to_v3_with_private_atomic_backup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let original = v2_state();
        write_private_file(&path, &original);

        let state = MemoryState::load(&path).expect("migrate v2 state");

        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.generation, 42);
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.schema_migrated_from, Some(2));
        let metadata = state.records.get(ID).expect("migrated metadata");
        assert!(metadata.pinned);
        assert!(!metadata.locked);
        assert_eq!(metadata.expires_at_ms, Some(200));
        assert_eq!(metadata.feedback.used, 1);
        assert!(metadata.confidence >= 0.0 && metadata.confidence <= 1.0);
        assert_eq!(metadata.superseded_by, None);
        assert!(metadata.supersedes.is_empty());
        assert!(metadata.conflict_with.is_empty());

        let backup = v2_backup_path(&path);
        assert_eq!(
            fs::read_to_string(&backup).expect("read v2 backup"),
            original
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(backup)
                    .expect("backup metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn v2_migration_does_not_overwrite_existing_v2_backup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v2_backup_path(&path);
        let original = v2_state();
        write_private_file(&path, &original);
        write_private_file(&backup, &original);

        let state = MemoryState::load(&path).expect("migrate v2 with exact backup");

        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(
            fs::read_to_string(backup).expect("read exact v2 backup"),
            original
        );
    }

    #[test]
    fn v2_migration_rejects_partial_backup_without_modifying_state() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        let backup = v2_backup_path(&path);
        let original = v2_state();
        write_private_file(&path, &original);
        write_private_file(&backup, &original.as_bytes()[..original.len() / 2]);

        let error = MemoryState::load(&path).expect_err("reject partial v2 backup");

        assert!(error.to_string().contains("does not exactly match"));
        assert_state_unchanged(&path, &original);
    }

    fn v3_state_with_relations() -> MemoryState {
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
        let mut state = v3_state_with_relations();
        // Break reciprocity: set A.superseded_by = B but remove A from B.supersedes.
        let b = other_id().to_string();
        if let Some(meta) = state.records.get_mut(&b) {
            meta.supersedes.clear();
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_dangling_superseded_by() {
        let mut state = v3_state_with_relations();
        let dangling = "mem_ffffffffffffffffffffffffffffffff".to_string();
        if let Some(meta) = state.records.get_mut(ID) {
            meta.superseded_by = Some(dangling);
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_self_supersession() {
        let mut state = v3_state_with_relations();
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

    #[test]
    #[ignore = "release-only perf benchmark for v2->v3 transform and chain resolution"]
    fn perf_v2_transform_and_chain_resolution() {
        use std::time::Instant;

        let n: usize = 10_000;
        let mut records = serde_json::Map::new();
        for i in 0..n {
            let id = format!("mem_{i:032}");
            records.insert(
                id,
                json!({
                    "scope": "project",
                    "scope_key": null,
                    "origin": "manual",
                    "expires_at_ms": null,
                    "half_life_days": 180.0,
                    "code_anchors": [],
                    "feedback": {"injected": 0, "used": 0, "ignored": 0, "error": 0},
                    "shared_source": null,
                    "pinned": false,
                    "locked": false,
                    "lock_reason": null
                }),
            );
        }
        let v2_json = serde_json::to_string(&json!({
            "schema_version": 2,
            "generation": 1,
            "records": records,
            "tombstones": {},
            "retrievals": {},
            "pending_deletes": []
        }))
        .expect("serialize v2 perf fixture");

        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("state.json");
        write_private_file(&path, &v2_json);

        let start = Instant::now();
        let state = MemoryState::load(&path).expect("migrate v2 perf state");
        let elapsed_ms = start.elapsed().as_millis();
        assert_eq!(state.records.len(), n);
        assert!(
            elapsed_ms < 500,
            "v2->v3 transform of {n} records took {elapsed_ms}ms (budget 500ms)"
        );

        let chain_len: usize = 1_000;
        let mut chain_state = MemoryState::default();
        for i in 0..chain_len {
            let id = format!("mem_{i:032}");
            let next_id = if i + 1 < chain_len {
                Some(format!("mem_{:032}", i + 1))
            } else {
                None
            };
            let prev_id = if i > 0 {
                vec![format!("mem_{:032}", i - 1)]
            } else {
                Vec::new()
            };
            chain_state.records.insert(
                id,
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
                    superseded_by: next_id,
                    supersedes: prev_id,
                    conflict_with: Vec::new(),
                },
            );
        }

        let start = Instant::now();
        for i in 0..chain_len {
            let mut current = format!("mem_{i:032}");
            for _ in 0..chain_len {
                match chain_state
                    .records
                    .get(&current)
                    .and_then(|m| m.superseded_by.clone())
                {
                    Some(next) => current = next,
                    None => break,
                }
            }
        }
        let total_ns = start.elapsed().as_nanos() / u128::try_from(chain_len).unwrap_or(1);
        #[allow(clippy::cast_precision_loss)]
        let link_ms = total_ns as f64 / 1_000_000.0;
        assert!(
            link_ms < 1.0,
            "chain resolution average {link_ms:.4}ms/link (budget 1ms)"
        );
    }
}
