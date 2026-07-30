use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::MemoryConfig;
use crate::storage::atomic::write_json_atomic;

const ACTIVE_EMBEDDING_FORMAT_VERSION: u32 = 1;
const ACTIVE_EMBEDDING_MAX_BYTES: usize = 64 * 1024;

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
                && self.profile_fingerprint.len() == 64
                && self.embedding_dimension > 0
                && self.activated_at_ms >= 0,
            "active embedding pointer is incomplete"
        );
        Ok(())
    }
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
}
