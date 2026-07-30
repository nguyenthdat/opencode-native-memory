use serde::{Deserialize, Serialize};

use crate::MemoryConfig;
use crate::config::{EmbeddingConfig, hash_hex};
use crate::embedding_generation::ActiveEmbedding;

const CURRENT_PROFILE_ID: &str = "qwen3-text-4b-q4";
const CURRENT_PROFILE_REPOSITORY: &str = "Qwen/Qwen3-Embedding-4B-GGUF";
const CURRENT_PROFILE_FILENAME: &str = "Qwen3-Embedding-4B-Q4_K_M.gguf";
const CURRENT_PROFILE_REVISION: &str = "f4602530db1d980e16da9d7d3a70294cf5c190be";
const CATALOG_IDENTITY: &str = concat!(
    "model-catalog-v1\n",
    "qwen3-text-4b-q4:f4602530db1d980e16da9d7d3a70294cf5c190be:",
    "2b0cf8f17b4c723c27303015383c27ec4bf2d8314bb677d05e920dd70bb0f16b\n",
    "qwen3-text-0.6b-q8:370f27d7550e0def9b39c1f16d3fbaa13aa67728:",
    "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439\n",
    "qwen3-text-8b-q4:69d0e58a13e463cd99a9b83e3f5fee7c10265fab:",
    "3fcd3febec8b3fd64435204db75bf0dd73b91e8d0661e0331acfe7e7c3120b85\n",
    "bge-m3\n",
    "nomic-embed-text-v1.5\n",
    "qwen3-vl-embedding-2b\n",
    "qwen3-vl-embedding-8b\n",
);

#[derive(Debug, Clone, Serialize)]
pub struct ModelProfile {
    pub profile_id: String,
    pub display_name: String,
    pub description: String,
    pub modalities: Vec<String>,
    pub repository: Option<String>,
    pub filename: Option<String>,
    pub revision: Option<String>,
    pub artifact_sha256: Option<String>,
    pub runtime_family: String,
    pub dimension: Option<usize>,
    pub metric: Option<String>,
    pub support_level: String,
    pub selectable: bool,
    pub default_for_new_projects: bool,
    pub recommended: bool,
    pub installed: bool,
    pub platform_supported: bool,
    pub runtime_available: bool,
    pub artifact_locked: bool,
    pub estimated_download_bytes: Option<u64>,
    pub estimated_resident_bytes: Option<u64>,
    pub unavailable_reason: Option<ModelProfileReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelProfileReason {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelProfilesResponse {
    pub catalog_version: u32,
    pub catalog_digest: String,
    pub active_profile_id: String,
    pub active_generation_id: String,
    pub profiles: Vec<ModelProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSwitchRequest {
    pub target_profile_id: String,
    #[serde(default)]
    pub switch_id: Option<String>,
    #[serde(default)]
    pub expected_active_profile_id: Option<String>,
    #[serde(default)]
    pub allow_dense_downtime: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub force_rebuild: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSwitchBlocker {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSwitchPreflight {
    pub can_start: bool,
    pub availability: String,
    pub dense_search_available: bool,
    pub estimated_download_bytes: Option<u64>,
    pub estimated_disk_bytes: Option<u64>,
    pub estimated_resident_bytes: Option<u64>,
    pub warnings: Vec<String>,
    pub blockers: Vec<ModelSwitchBlocker>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSwitchResponse {
    pub switch_id: Option<String>,
    pub dry_run: bool,
    pub state: String,
    pub active_profile_id: String,
    pub target_profile_id: String,
    pub active_generation_id: String,
    pub target_generation_id: Option<String>,
    pub dense_search_available: bool,
    pub preflight: ModelSwitchPreflight,
}

pub fn profiles(config: &MemoryConfig) -> anyhow::Result<ModelProfilesResponse> {
    let embedding = config.embedding();
    let persisted = ActiveEmbedding::load(&config.active_embedding_path(), config.project_id())?;
    let active_profile_id = persisted.as_ref().map_or_else(
        || configured_profile_id(embedding),
        |active| active.profile_id.clone(),
    );
    let installed_current =
        active_profile_id == CURRENT_PROFILE_ID && current_profile_installed(config.model_cache());
    let mut profiles = vec![
        current_qwen_profile(installed_current),
        qwen_06b_profile(),
        qwen_8b_profile(),
        bge_m3_profile(),
        nomic_profile(),
        qwen_vl_profile(
            "qwen3-vl-embedding-2b",
            "Qwen3-VL Embedding 2B",
            "Qwen/Qwen3-VL-Embedding-2B",
            4_270_000_000,
        ),
        qwen_vl_profile(
            "qwen3-vl-embedding-8b",
            "Qwen3-VL Embedding 8B",
            "Qwen/Qwen3-VL-Embedding-8B",
            16_300_000_000,
        ),
    ];
    if active_profile_id == "legacy-custom" {
        profiles.insert(0, custom_profile(embedding));
    }
    Ok(ModelProfilesResponse {
        catalog_version: 1,
        catalog_digest: catalog_digest(),
        active_profile_id,
        active_generation_id: persisted
            .map_or_else(|| "legacy".to_string(), |active| active.generation_id),
        profiles,
    })
}

fn custom_profile(config: &EmbeddingConfig) -> ModelProfile {
    ModelProfile {
        profile_id: "legacy-custom".to_string(),
        display_name: "Legacy custom embedding".to_string(),
        description:
            "Active development override retained without being relabelled as a built-in profile."
                .to_string(),
        modalities: vec!["text".to_string()],
        repository: Some(config.repo.clone()),
        filename: Some(config.filename.clone()),
        revision: Some(config.revision.clone()),
        artifact_sha256: None,
        runtime_family: "llama.cpp-gguf-text".to_string(),
        dimension: config.dimension,
        metric: Some("cosine".to_string()),
        support_level: "preview".to_string(),
        selectable: false,
        default_for_new_projects: false,
        recommended: false,
        installed: config
            .model_path
            .as_deref()
            .is_some_and(std::path::Path::is_file),
        platform_supported: true,
        runtime_available: true,
        artifact_locked: false,
        estimated_download_bytes: None,
        estimated_resident_bytes: None,
        unavailable_reason: Some(ModelProfileReason {
            code: "development_override".to_string(),
            message: "Custom profiles require an explicit generation migration before switching."
                .to_string(),
        }),
    }
}

pub fn preflight(
    config: &MemoryConfig,
    request: &ModelSwitchRequest,
) -> anyhow::Result<ModelSwitchResponse> {
    let catalog = profiles(config)?;
    let target = catalog
        .profiles
        .iter()
        .find(|profile| profile.profile_id == request.target_profile_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "PROFILE_NOT_FOUND: unknown embedding profile {}",
                request.target_profile_id
            )
        })?;
    let mut blockers = Vec::new();
    let mut warnings = vec![
        "This phase exposes preflight only; generation migration is not enabled yet.".to_string(),
    ];
    let availability = if request.allow_dense_downtime {
        "allow_dense_downtime"
    } else {
        "keep_old_dense"
    };

    if let Some(expected) = &request.expected_active_profile_id
        && expected != &catalog.active_profile_id
    {
        blockers.push(ModelSwitchBlocker {
            code: "ACTIVE_PROFILE_MISMATCH".to_string(),
            message: format!(
                "expected active profile {expected}, found {}",
                catalog.active_profile_id
            ),
        });
    }
    if !target.selectable {
        let reason = target
            .unavailable_reason
            .as_ref()
            .map(|reason| reason.message.clone())
            .unwrap_or_else(|| "profile is not selectable".to_string());
        blockers.push(ModelSwitchBlocker {
            code: "PROFILE_UNSUPPORTED".to_string(),
            message: reason,
        });
    }
    if target.profile_id == catalog.active_profile_id && !request.force_rebuild {
        blockers.push(ModelSwitchBlocker {
            code: "ACTIVE_PROFILE_NOOP".to_string(),
            message: "target profile is already active; use force_rebuild to preflight a rebuild"
                .to_string(),
        });
    }
    if request.allow_dense_downtime {
        warnings.push(
            "Dense search downtime requires a future generation migration implementation."
                .to_string(),
        );
    }
    blockers.push(ModelSwitchBlocker {
        code: "GENERATION_MIGRATION_NOT_ENABLED".to_string(),
        message:
            "target collection generations and durable switch jobs are not enabled in this phase"
                .to_string(),
    });

    Ok(ModelSwitchResponse {
        switch_id: request.switch_id.clone(),
        dry_run: request.dry_run,
        state: "preflight".to_string(),
        active_profile_id: catalog.active_profile_id,
        target_profile_id: target.profile_id.clone(),
        active_generation_id: catalog.active_generation_id,
        target_generation_id: None,
        dense_search_available: true,
        preflight: ModelSwitchPreflight {
            can_start: blockers.is_empty(),
            availability: availability.to_string(),
            dense_search_available: true,
            estimated_download_bytes: target.estimated_download_bytes,
            estimated_disk_bytes: target.estimated_download_bytes,
            estimated_resident_bytes: target.estimated_resident_bytes,
            warnings,
            blockers,
        },
    })
}

pub(crate) fn configured_profile_id(config: &EmbeddingConfig) -> String {
    let default = EmbeddingConfig::default();
    if config.model_path.is_none()
        && config.repo == default.repo
        && config.filename == default.filename
        && config.revision == default.revision
        && config.pooling == default.pooling
        && config.attention == default.attention
        && config.query_template == default.query_template
        && config.passage_template == default.passage_template
        && config.add_bos == default.add_bos
        && config.append_eos == default.append_eos
        && config.normalize == default.normalize
        && config.dimension == default.dimension
        && config.context_size == default.context_size
    {
        CURRENT_PROFILE_ID.to_string()
    } else {
        "legacy-custom".to_string()
    }
}

fn current_profile_installed(model_cache: &std::path::Path) -> bool {
    model_cache
        .join(format!(
            "models--{}",
            CURRENT_PROFILE_REPOSITORY.replace('/', "--")
        ))
        .join("snapshots")
        .join(CURRENT_PROFILE_REVISION)
        .join(CURRENT_PROFILE_FILENAME)
        .is_file()
}

fn current_qwen_profile(installed: bool) -> ModelProfile {
    ModelProfile {
        profile_id: CURRENT_PROFILE_ID.to_string(),
        display_name: "Qwen3 Embedding 4B Q4_K_M".to_string(),
        description: "Current high-quality local text/code embedding default.".to_string(),
        modalities: vec!["text".to_string()],
        repository: Some(CURRENT_PROFILE_REPOSITORY.to_string()),
        filename: Some(CURRENT_PROFILE_FILENAME.to_string()),
        revision: Some(CURRENT_PROFILE_REVISION.to_string()),
        artifact_sha256: Some(
            "2b0cf8f17b4c723c27303015383c27ec4bf2d8314bb677d05e920dd70bb0f16b".to_string(),
        ),
        runtime_family: "llama.cpp-gguf-text".to_string(),
        dimension: Some(2560),
        metric: Some("cosine".to_string()),
        support_level: "stable".to_string(),
        selectable: true,
        default_for_new_projects: true,
        recommended: true,
        installed,
        platform_supported: true,
        runtime_available: true,
        artifact_locked: true,
        estimated_download_bytes: Some(2_496_703_776),
        estimated_resident_bytes: Some(8_000_000_000),
        unavailable_reason: None,
    }
}

fn qwen_06b_profile() -> ModelProfile {
    gguf_preview(
        "qwen3-text-0.6b-q8",
        "Qwen3 Embedding 0.6B Q8_0",
        "Qwen/Qwen3-Embedding-0.6B-GGUF",
        "Qwen3-Embedding-0.6B-Q8_0.gguf",
        1024,
        639_150_592,
    )
}

fn qwen_8b_profile() -> ModelProfile {
    gguf_preview(
        "qwen3-text-8b-q4",
        "Qwen3 Embedding 8B Q4_K_M",
        "Qwen/Qwen3-Embedding-8B-GGUF",
        "Qwen3-Embedding-8B-Q4_K_M.gguf",
        4096,
        4_676_804_928,
    )
}

fn gguf_preview(
    profile_id: &str,
    display_name: &str,
    repository: &str,
    filename: &str,
    dimension: usize,
    download_bytes: u64,
) -> ModelProfile {
    ModelProfile {
        profile_id: profile_id.to_string(),
        display_name: display_name.to_string(),
        description:
            "Popular GGUF text embedding preset; artifact and migration gates are pending."
                .to_string(),
        modalities: vec!["text".to_string()],
        repository: Some(repository.to_string()),
        filename: Some(filename.to_string()),
        revision: Some(
            match profile_id {
                "qwen3-text-0.6b-q8" => "370f27d7550e0def9b39c1f16d3fbaa13aa67728",
                "qwen3-text-8b-q4" => "69d0e58a13e463cd99a9b83e3f5fee7c10265fab",
                _ => unreachable!("unknown GGUF preview profile"),
            }
            .to_string(),
        ),
        artifact_sha256: Some(
            match profile_id {
                "qwen3-text-0.6b-q8" => {
                    "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439"
                }
                "qwen3-text-8b-q4" => {
                    "3fcd3febec8b3fd64435204db75bf0dd73b91e8d0661e0331acfe7e7c3120b85"
                }
                _ => unreachable!("unknown GGUF preview profile"),
            }
            .to_string(),
        ),
        runtime_family: "llama.cpp-gguf-text".to_string(),
        dimension: Some(dimension),
        metric: Some("cosine".to_string()),
        support_level: "preview".to_string(),
        selectable: false,
        default_for_new_projects: false,
        recommended: false,
        installed: false,
        platform_supported: true,
        runtime_available: true,
        artifact_locked: true,
        estimated_download_bytes: Some(download_bytes),
        estimated_resident_bytes: None,
        unavailable_reason: Some(ModelProfileReason {
            code: "generation_migration_not_enabled".to_string(),
            message: "Artifact is locked, but generation migration and retrieval-quality gates are not enabled yet.".to_string(),
        }),
    }
}

fn bge_m3_profile() -> ModelProfile {
    unsupported_text_profile(
        "bge-m3",
        "BGE-M3",
        "BAAI/bge-m3",
        "Popular multilingual text profile; current package has no validated llama.cpp GGUF runtime.",
    )
}

fn nomic_profile() -> ModelProfile {
    unsupported_text_profile(
        "nomic-embed-text-v1.5",
        "Nomic Embed Text v1.5",
        "nomic-ai/nomic-embed-text-v1.5",
        "Popular English text profile; current package has no validated local runtime for this artifact.",
    )
}

fn unsupported_text_profile(
    profile_id: &str,
    display_name: &str,
    repository: &str,
    description: &str,
) -> ModelProfile {
    ModelProfile {
        profile_id: profile_id.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        modalities: vec!["text".to_string()],
        repository: Some(repository.to_string()),
        filename: None,
        revision: None,
        artifact_sha256: None,
        runtime_family: "unvalidated".to_string(),
        dimension: None,
        metric: None,
        support_level: "preview".to_string(),
        selectable: false,
        default_for_new_projects: false,
        recommended: false,
        installed: false,
        platform_supported: false,
        runtime_available: false,
        artifact_locked: false,
        estimated_download_bytes: None,
        estimated_resident_bytes: None,
        unavailable_reason: Some(ModelProfileReason {
            code: "runtime_unavailable".to_string(),
            message: description.to_string(),
        }),
    }
}

fn qwen_vl_profile(
    profile_id: &str,
    display_name: &str,
    repository: &str,
    download_bytes: u64,
) -> ModelProfile {
    ModelProfile {
        profile_id: profile_id.to_string(),
        display_name: display_name.to_string(),
        description: "Experimental unified text, image, and mixed-input candidate.".to_string(),
        modalities: vec!["text".to_string(), "image".to_string(), "mixed".to_string()],
        repository: Some(repository.to_string()),
        filename: None,
        revision: None,
        artifact_sha256: None,
        runtime_family: "qwen3-vl-unvalidated".to_string(),
        dimension: None,
        metric: None,
        support_level: "unsupported".to_string(),
        selectable: false,
        default_for_new_projects: false,
        recommended: false,
        installed: false,
        platform_supported: false,
        runtime_available: false,
        artifact_locked: false,
        estimated_download_bytes: Some(download_bytes),
        estimated_resident_bytes: None,
        unavailable_reason: Some(ModelProfileReason {
            code: "runtime_unavailable".to_string(),
            message: "No validated packaged multimodal embedding runtime or desktop memory gate is available.".to_string(),
        }),
    }
}

fn catalog_digest() -> String {
    hash_hex(CATALOG_IDENTITY.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        CURRENT_PROFILE_ID, ModelSwitchRequest, configured_profile_id, preflight, profiles,
    };
    use crate::{EmbeddingConfig, MemoryConfig};

    fn config() -> (tempfile::TempDir, MemoryConfig) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        (temp, config)
    }

    #[test]
    fn current_profile_is_the_only_stable_default() {
        let (_temp, config) = config();
        let response = profiles(&config).expect("profiles");
        assert_eq!(response.active_profile_id, CURRENT_PROFILE_ID);
        assert_eq!(
            response
                .profiles
                .iter()
                .filter(|profile| profile.selectable)
                .count(),
            1
        );
        assert!(response.profiles[0].default_for_new_projects);
        assert_eq!(response.catalog_digest.len(), 64);
        assert!(response.profiles[0].artifact_locked);
        assert!(response.profiles[0].artifact_sha256.is_some());
        assert!(!response.profiles[0].installed);
    }

    #[test]
    fn current_profile_is_installed_only_when_the_cached_artifact_exists() {
        let (_temp, config) = config();
        let artifact = config
            .model_cache()
            .join("models--Qwen--Qwen3-Embedding-4B-GGUF")
            .join("snapshots")
            .join("f4602530db1d980e16da9d7d3a70294cf5c190be")
            .join("Qwen3-Embedding-4B-Q4_K_M.gguf");
        fs::create_dir_all(artifact.parent().expect("artifact parent"))
            .expect("create model cache");
        fs::write(&artifact, b"model").expect("write cached artifact");

        assert!(profiles(&config).expect("profiles").profiles[0].installed);
    }

    #[test]
    fn unsupported_profile_is_visible_but_blocked_before_migration() {
        let (_temp, config) = config();
        let response = preflight(
            &config,
            &ModelSwitchRequest {
                target_profile_id: "qwen3-vl-embedding-8b".to_string(),
                switch_id: None,
                expected_active_profile_id: None,
                allow_dense_downtime: false,
                dry_run: true,
                force_rebuild: false,
            },
        )
        .unwrap();
        assert!(!response.preflight.can_start);
        assert!(
            response
                .preflight
                .blockers
                .iter()
                .any(|blocker| blocker.code == "PROFILE_UNSUPPORTED")
        );
    }

    #[test]
    fn custom_embedding_is_not_silently_relabelled() {
        let config = EmbeddingConfig {
            repo: "custom/model".to_string(),
            ..EmbeddingConfig::default()
        };
        assert_eq!(configured_profile_id(&config), "legacy-custom");

        let local_override = EmbeddingConfig {
            model_path: Some("custom.gguf".into()),
            ..EmbeddingConfig::default()
        };
        assert_eq!(configured_profile_id(&local_override), "legacy-custom");

        let runtime_tuned = EmbeddingConfig {
            threads: Some(4),
            gpu_layers: Some(99),
            ..EmbeddingConfig::default()
        };
        assert_eq!(configured_profile_id(&runtime_tuned), CURRENT_PROFILE_ID);
    }
}
