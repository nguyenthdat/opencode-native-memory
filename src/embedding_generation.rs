use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::MemoryConfig;
use crate::config::EmbeddingConfig;
use crate::storage::atomic::write_json_atomic;

const ACTIVE_EMBEDDING_FORMAT_VERSION: u32 = 1;
const ACTIVE_EMBEDDING_MAX_BYTES: usize = 64 * 1024;
const GENERATION_MANIFEST_FORMAT_VERSION: u32 = 1;
const MODEL_SWITCH_FORMAT_VERSION: u32 = 1;
const GENERATION_MANIFEST_MAX_BYTES: usize = 256 * 1024;
const MODEL_SWITCH_MAX_BYTES: usize = 1024 * 1024;
const MAX_SWITCH_HISTORY: usize = 16;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveEmbedding {
    pub format_version: u32,
    pub project_id: String,
    pub generation_id: String,
    pub profile_id: String,
    pub profile_fingerprint: String,
    pub embedding_dimension: usize,
    pub activated_at_ms: i64,
    #[serde(default)]
    pub predecessor_generation_id: Option<String>,
    pub source_state_revision: u64,
}

impl ActiveEmbedding {
    pub(crate) fn load(path: &Path, project_id: &str) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read active embedding pointer {}", path.display()))?;
        ensure!(
            bytes.len() <= ACTIVE_EMBEDDING_MAX_BYTES,
            "active embedding pointer exceeds size limit"
        );
        let active: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid active embedding pointer {}", path.display()))?;
        active.validate(project_id)?;
        Ok(Some(active))
    }

    pub(crate) fn load_or_initialize(
        config: &MemoryConfig,
        profile_id: &str,
        embedding_dimension: usize,
        source_state_revision: u64,
        now_ms: i64,
    ) -> Result<Self> {
        let profile_fingerprint = config.embedding_profile_fingerprint()?;
        if let Some(active) = Self::load(&config.active_embedding_path(), config.project_id())? {
            ensure!(
                active.profile_id == profile_id
                    && active.profile_fingerprint == profile_fingerprint
                    && active.embedding_dimension == embedding_dimension,
                "active embedding generation does not match the configured profile; use model switch generation migration"
            );
            return Ok(active);
        }
        let active = Self {
            format_version: ACTIVE_EMBEDDING_FORMAT_VERSION,
            project_id: config.project_id().to_string(),
            generation_id: "legacy".to_string(),
            profile_id: profile_id.to_string(),
            profile_fingerprint,
            embedding_dimension,
            activated_at_ms: now_ms,
            predecessor_generation_id: None,
            source_state_revision,
        };
        active.validate(config.project_id())?;
        write_json_atomic(
            &config.active_embedding_path(),
            &active,
            ACTIVE_EMBEDDING_MAX_BYTES,
        )?;
        Ok(active)
    }

    pub(crate) fn install(&self, path: &Path, project_id: &str) -> Result<()> {
        self.validate(project_id)?;
        write_json_atomic(path, self, ACTIVE_EMBEDDING_MAX_BYTES)
    }

    fn validate(&self, project_id: &str) -> Result<()> {
        ensure!(
            self.format_version == ACTIVE_EMBEDDING_FORMAT_VERSION,
            "unsupported active embedding pointer version"
        );
        ensure!(
            self.project_id == project_id,
            "active embedding pointer belongs to a different project"
        );
        ensure!(
            !self.generation_id.trim().is_empty()
                && !self.profile_id.trim().is_empty()
                && (self.generation_id == "legacy"
                    || (self.generation_id.starts_with("gen_")
                        && self.generation_id[4..]
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')))
                && self.profile_fingerprint.len() == 64
                && self.embedding_dimension > 0
                && self.activated_at_ms >= 0,
            "active embedding pointer is incomplete"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GenerationState {
    Building,
    Complete,
    Active,
    Retained,
    Quarantined,
    Deleting,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddingGenerationManifest {
    pub format_version: u32,
    pub generation_id: String,
    pub project_id: String,
    pub profile_id: String,
    pub profile_fingerprint: String,
    pub artifact_sha256: String,
    pub runtime_identity: String,
    pub preprocessing_identity: String,
    pub embedding_dimension: usize,
    pub metric: String,
    pub normalization: bool,
    pub source_generation_id: String,
    pub source_state_revision: u64,
    #[serde(default)]
    pub source_content_digest: String,
    pub record_count: u64,
    pub state: GenerationState,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

impl EmbeddingGenerationManifest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn building(
        generation_id: String,
        project_id: String,
        profile_id: String,
        profile_fingerprint: String,
        artifact_sha256: String,
        embedding_dimension: usize,
        source_generation_id: String,
        source_state_revision: u64,
        source_content_digest: String,
        created_at_ms: i64,
    ) -> Self {
        Self {
            format_version: GENERATION_MANIFEST_FORMAT_VERSION,
            generation_id,
            project_id,
            profile_id,
            profile_fingerprint,
            artifact_sha256,
            runtime_identity: "llama.cpp-gguf-text-v1".to_string(),
            preprocessing_identity: "profile-catalog-v1".to_string(),
            embedding_dimension,
            metric: "cosine".to_string(),
            normalization: true,
            source_generation_id,
            source_state_revision,
            source_content_digest,
            record_count: 0,
            state: GenerationState::Building,
            created_at_ms,
            completed_at_ms: None,
        }
    }

    pub(crate) fn load(path: &Path, project_id: &str) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read generation manifest {}", path.display()))?;
        ensure!(
            bytes.len() <= GENERATION_MANIFEST_MAX_BYTES,
            "generation manifest exceeds size limit"
        );
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid generation manifest {}", path.display()))?;
        manifest.validate(project_id)?;
        Ok(manifest)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate(&self.project_id)?;
        write_json_atomic(path, self, GENERATION_MANIFEST_MAX_BYTES)
    }

    fn validate(&self, project_id: &str) -> Result<()> {
        ensure!(
            self.format_version == GENERATION_MANIFEST_FORMAT_VERSION,
            "unsupported generation manifest version"
        );
        ensure!(
            self.project_id == project_id,
            "generation belongs to a different project"
        );
        ensure!(
            !self.generation_id.is_empty()
                && !self.profile_id.is_empty()
                && self.generation_id.starts_with("gen_")
                && self
                    .generation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && self.profile_fingerprint.len() == 64
                && self.artifact_sha256.len() == 64
                && self.embedding_dimension > 0
                && self.created_at_ms >= 0,
            "generation manifest is incomplete"
        );
        ensure!(
            self.completed_at_ms
                .is_none_or(|completed| completed >= self.created_at_ms),
            "generation completion timestamp is invalid"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SwitchPhase {
    Queued,
    Validating,
    Downloading,
    Preparing,
    Reindexing,
    Verifying,
    Committing,
    Succeeded,
    CancelRequested,
    Cancelled,
    Failed,
}

impl SwitchPhase {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Cancelled | Self::Failed)
    }

    pub(crate) const fn can_cancel(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Validating
                | Self::Downloading
                | Self::Preparing
                | Self::Reindexing
                | Self::Verifying
                | Self::CancelRequested
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSwitchJournal {
    pub switch_id: String,
    pub request_digest: String,
    pub source_generation_id: String,
    pub source_profile_id: String,
    #[serde(default)]
    pub source_embedding: Option<EmbeddingConfig>,
    #[serde(default)]
    pub target_embedding: Option<EmbeddingConfig>,
    pub target_generation_id: String,
    pub target_profile_id: String,
    pub phase: SwitchPhase,
    pub cursor: usize,
    pub completed_records: u64,
    pub total_records: u64,
    pub cancel_requested: bool,
    pub source_state_revision: u64,
    #[serde(default)]
    pub source_content_digest: String,
    pub allow_dense_downtime: bool,
    pub retain_previous: bool,
    #[serde(default)]
    pub target_preexisting: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSwitchStore {
    format_version: u32,
    project_id: String,
    pub current: Option<ModelSwitchJournal>,
    #[serde(default)]
    pub history: Vec<ModelSwitchJournal>,
}

impl ModelSwitchStore {
    pub(crate) fn load(path: &Path, project_id: &str) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                format_version: MODEL_SWITCH_FORMAT_VERSION,
                project_id: project_id.to_string(),
                current: None,
                history: Vec::new(),
            });
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read model switch journal {}", path.display()))?;
        ensure!(
            bytes.len() <= MODEL_SWITCH_MAX_BYTES,
            "model switch journal exceeds size limit"
        );
        let store: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid model switch journal {}", path.display()))?;
        store.validate(project_id)?;
        Ok(store)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate(&self.project_id)?;
        write_json_atomic(path, self, MODEL_SWITCH_MAX_BYTES)
    }

    pub(crate) fn find(&self, switch_id: &str) -> Option<&ModelSwitchJournal> {
        self.current
            .as_ref()
            .filter(|job| job.switch_id == switch_id)
            .or_else(|| self.history.iter().find(|job| job.switch_id == switch_id))
    }

    pub(crate) fn replace_current(&mut self, job: ModelSwitchJournal) {
        if let Some(previous) = self.current.replace(job)
            && previous.phase.is_terminal()
        {
            self.history.push(previous);
            if self.history.len() > MAX_SWITCH_HISTORY {
                self.history
                    .drain(..self.history.len() - MAX_SWITCH_HISTORY);
            }
        }
    }

    fn validate(&self, project_id: &str) -> Result<()> {
        ensure!(
            self.format_version == MODEL_SWITCH_FORMAT_VERSION,
            "unsupported model switch journal version"
        );
        ensure!(
            self.project_id == project_id,
            "model switch journal belongs to a different project"
        );
        ensure!(
            self.history.len() <= MAX_SWITCH_HISTORY,
            "model switch history exceeds limit"
        );
        for job in self.current.iter().chain(&self.history) {
            ensure!(
                !job.switch_id.is_empty()
                    && !job.source_generation_id.is_empty()
                    && !job.target_generation_id.is_empty()
                    && !job.source_profile_id.is_empty()
                    && !job.target_profile_id.is_empty()
                    && job.request_digest.len() == 64
                    && job.completed_records <= job.total_records,
                "model switch job is invalid"
            );
        }
        Ok(())
    }
}

pub(crate) fn generation_manifest_path(config: &MemoryConfig, generation_id: &str) -> PathBuf {
    config
        .embedding_generation_dir(generation_id)
        .join("generation.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_and_reopens_the_legacy_generation_atomically() {
        let temp = tempfile::tempdir().expect("temp");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        std::fs::create_dir_all(config.project_data_dir()).expect("project data");
        let first =
            ActiveEmbedding::load_or_initialize(&config, "profile", 32, 7, 10).expect("initialize");
        let reopened =
            ActiveEmbedding::load_or_initialize(&config, "profile", 32, 8, 20).expect("reopen");

        assert_eq!(first.generation_id, "legacy");
        assert_eq!(reopened.source_state_revision, 7);
        assert!(config.active_embedding_path().is_file());
    }

    #[test]
    fn rejects_a_profile_change_without_generation_migration() {
        let temp = tempfile::tempdir().expect("temp");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        std::fs::create_dir_all(config.project_data_dir()).expect("project data");
        ActiveEmbedding::load_or_initialize(&config, "profile-a", 32, 0, 1).expect("initialize");

        let error = ActiveEmbedding::load_or_initialize(&config, "profile-b", 64, 0, 2)
            .expect_err("reject implicit profile change");
        assert!(error.to_string().contains("generation migration"));
    }

    #[test]
    fn switch_store_keeps_terminal_history_and_rejects_invalid_shape() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("model-switch.json");
        let mut store = ModelSwitchStore::load(&path, "project").expect("load empty store");
        let job = |id: &str| ModelSwitchJournal {
            switch_id: id.to_string(),
            request_digest: "a".repeat(64),
            source_generation_id: "legacy".to_string(),
            source_profile_id: "profile-a".to_string(),
            source_embedding: Some(EmbeddingConfig::default()),
            target_embedding: None,
            target_generation_id: format!("gen_{id}"),
            target_profile_id: "profile-b".to_string(),
            phase: SwitchPhase::Succeeded,
            cursor: 1,
            completed_records: 1,
            total_records: 1,
            cancel_requested: false,
            source_state_revision: 0,
            source_content_digest: "b".repeat(64),
            allow_dense_downtime: false,
            retain_previous: true,
            target_preexisting: false,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
            error_code: None,
            error_message: None,
        };
        store.replace_current(job("switch-a"));
        store.replace_current(job("switch-b"));
        store.save(&path).expect("save history");

        let reopened = ModelSwitchStore::load(&path, "project").expect("reload history");
        assert!(reopened.find("switch-a").is_some());
        assert!(reopened.find("switch-b").is_some());
    }
}
