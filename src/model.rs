use serde::{Deserialize, Serialize};

use crate::MemoryConfig;
use crate::config::{EmbeddingConfig, hash_hex};
use crate::embedding_generation::{
    ActiveEmbedding, EmbeddingGenerationManifest, GenerationState, ModelSwitchStore, SwitchPhase,
    generation_manifest_path,
};
use crate::storage::state::MemoryState;

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

const QWEN_06B_PROFILE_ID: &str = "qwen3-text-0.6b-q8";
const QWEN_8B_PROFILE_ID: &str = "qwen3-text-8b-q4";
const DEFAULT_MODEL_MEMORY_BUDGET_BYTES: u64 = 16 * 1024 * 1024 * 1024;

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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelSwitchRequest {
    pub target_profile_id: String,
    #[serde(default)]
    pub switch_id: Option<String>,
    #[serde(default)]
    pub expected_active_profile_id: Option<String>,
    #[serde(default)]
    pub expected_active_generation_id: Option<String>,
    #[serde(default)]
    pub allow_dense_downtime: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub force_rebuild: bool,
    #[serde(default = "default_true")]
    pub retain_previous: bool,
    #[serde(default)]
    pub target_generation_id: Option<String>,
}

const fn default_true() -> bool {
    true
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

#[derive(Debug, Clone, Serialize)]
pub struct ModelSwitchStatusResponse {
    pub switch_id: String,
    pub state: String,
    pub active_profile_id: String,
    pub target_profile_id: String,
    pub active_generation_id: String,
    pub target_generation_id: Option<String>,
    pub completed_records: u64,
    pub total_records: u64,
    pub fraction: f64,
    pub cancel_requested: bool,
    pub dense_search_available: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub error: Option<ModelProfileReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSwitchCancelResponse {
    pub switch_id: String,
    pub outcome: String,
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
        qwen_06b_profile(profile_installed(config.model_cache(), QWEN_06B_PROFILE_ID)),
        qwen_8b_profile(profile_installed(config.model_cache(), QWEN_8B_PROFILE_ID)),
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
    let legacy_snapshot = if request.target_generation_id.as_deref() == Some("legacy") {
        let store = ModelSwitchStore::load(&config.model_switch_path(), config.project_id())?;
        store
            .current
            .iter()
            .chain(store.history.iter().rev())
            .find(|job| {
                job.phase == SwitchPhase::Succeeded
                    && job.source_generation_id == "legacy"
                    && job.source_profile_id == request.target_profile_id
            })
            .cloned()
    } else {
        None
    };
    let target = catalog
        .profiles
        .iter()
        .find(|profile| profile.profile_id == request.target_profile_id)
        .cloned()
        .or_else(|| {
            legacy_snapshot
                .as_ref()
                .and_then(|job| job.source_embedding.as_ref())
                .map(|embedding| {
                    let mut profile = custom_profile(embedding);
                    profile.profile_id = request.target_profile_id.clone();
                    profile.selectable = true;
                    profile.unavailable_reason = None;
                    profile
                })
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "PROFILE_NOT_FOUND: unknown embedding profile {}",
                request.target_profile_id
            )
        })?;
    let mut target = target;
    if legacy_snapshot.is_some() && target.profile_id == "legacy-custom" {
        target.selectable = true;
        target.unavailable_reason = None;
    }
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
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
    if let Some(expected) = &request.expected_active_generation_id
        && expected != &catalog.active_generation_id
    {
        blockers.push(ModelSwitchBlocker {
            code: "ACTIVE_PROFILE_MISMATCH".to_string(),
            message: format!(
                "expected active generation {expected}, found {}",
                catalog.active_generation_id
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
    if request.target_generation_id.is_none()
        && target.profile_id == catalog.active_profile_id
        && !request.force_rebuild
    {
        blockers.push(ModelSwitchBlocker {
            code: "ACTIVE_PROFILE_NOOP".to_string(),
            message: "target profile is already active; use force_rebuild to preflight a rebuild"
                .to_string(),
        });
    }
    if let Some(generation_id) = &request.target_generation_id {
        if !valid_generation_id(generation_id) {
            blockers.push(ModelSwitchBlocker {
                code: "TARGET_GENERATION_INVALID".to_string(),
                message: format!("target generation ID {generation_id} is invalid"),
            });
        } else if generation_id == "legacy" {
            let state = MemoryState::load(&config.state_path())?;
            let digest = state.canonical_revision_digest()?;
            let available = legacy_snapshot.as_ref().is_some_and(|job| {
                job.source_content_digest == digest && job.source_embedding.is_some()
            }) && config.collection_dir().is_dir()
                && config.project_data_dir().join("manifest.json").is_file();
            if !available {
                blockers.push(ModelSwitchBlocker {
                    code: "TARGET_GENERATION_NOT_FOUND".to_string(),
                    message: "retained legacy generation is unavailable or stale".to_string(),
                });
            }
        } else {
            let path = generation_manifest_path(config, generation_id);
            match EmbeddingGenerationManifest::load(&path, config.project_id()) {
                Ok(manifest) => {
                    if manifest.profile_id != request.target_profile_id {
                        blockers.push(ModelSwitchBlocker {
                            code: "TARGET_GENERATION_PROFILE_MISMATCH".to_string(),
                            message: format!(
                                "target generation belongs to {}, not {}",
                                manifest.profile_id, request.target_profile_id
                            ),
                        });
                    }
                    if !matches!(
                        manifest.state,
                        GenerationState::Retained | GenerationState::Complete
                    ) {
                        blockers.push(ModelSwitchBlocker {
                            code: "TARGET_GENERATION_UNAVAILABLE".to_string(),
                            message: "target generation is not retained or complete".to_string(),
                        });
                    }
                }
                Err(_) => blockers.push(ModelSwitchBlocker {
                    code: "TARGET_GENERATION_NOT_FOUND".to_string(),
                    message: format!("target generation {generation_id} was not found"),
                }),
            }
        }
    }
    if request.allow_dense_downtime {
        warnings.push(
            "Dense retrieval may use lexical fallback while the target worker is active."
                .to_string(),
        );
    }
    let estimated_download_bytes = if target.installed {
        Some(0)
    } else {
        target.estimated_download_bytes
    };
    let current_collection_path = if catalog.active_generation_id == "legacy" {
        config.collection_dir()
    } else {
        config.embedding_generation_dir(&catalog.active_generation_id)
    };
    let current_collection_bytes = directory_size(&current_collection_path).unwrap_or(0);
    let estimated_disk_bytes = estimated_download_bytes
        .map(|download| download.saturating_add(current_collection_bytes.max(64 * 1024 * 1024)));
    if let Some(required) = estimated_disk_bytes
        && let Some(parent) = config
            .project_data_dir()
            .parent()
            .filter(|path| path.exists())
        && fs2::available_space(parent).is_ok_and(|available| available < required)
    {
        blockers.push(ModelSwitchBlocker {
            code: "INSUFFICIENT_DISK".to_string(),
            message: format!("model switch needs about {required} bytes of free disk space"),
        });
    }
    let estimated_resident_bytes = target.estimated_resident_bytes;
    let budget = std::env::var("OPENCODE_MEMORY_MODEL_MEMORY_BUDGET_BYTES").map_or(
        Ok(DEFAULT_MODEL_MEMORY_BUDGET_BYTES),
        |value| {
            value.parse::<u64>().map_err(|_| {
                anyhow::anyhow!(
                    "OPENCODE_MEMORY_MODEL_MEMORY_BUDGET_BYTES must be a positive integer"
                )
            })
        },
    )?;
    let active_resident = catalog
        .profiles
        .iter()
        .find(|profile| profile.profile_id == catalog.active_profile_id)
        .and_then(|profile| profile.estimated_resident_bytes)
        .unwrap_or(0);
    let required =
        estimated_resident_bytes
            .unwrap_or(0)
            .saturating_add(if request.allow_dense_downtime {
                0
            } else {
                active_resident
            });
    if required > budget {
        blockers.push(ModelSwitchBlocker {
            code: "INSUFFICIENT_MEMORY".to_string(),
            message: format!(
                "model switch requires about {required} resident bytes, exceeding configured budget {budget}"
            ),
        });
    }

    Ok(ModelSwitchResponse {
        switch_id: request.switch_id.clone(),
        dry_run: request.dry_run,
        state: "preflight".to_string(),
        active_profile_id: catalog.active_profile_id,
        target_profile_id: target.profile_id.clone(),
        active_generation_id: catalog.active_generation_id,
        target_generation_id: request.target_generation_id.clone(),
        dense_search_available: true,
        preflight: ModelSwitchPreflight {
            can_start: blockers.is_empty(),
            availability: availability.to_string(),
            dense_search_available: true,
            estimated_download_bytes,
            estimated_disk_bytes,
            estimated_resident_bytes,
            warnings,
            blockers,
        },
    })
}

fn valid_generation_id(value: &str) -> bool {
    value == "legacy"
        || value.len() > 4
            && value.starts_with("gen_")
            && value[4..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(crate) fn configured_profile_id(config: &EmbeddingConfig) -> String {
    [CURRENT_PROFILE_ID, QWEN_06B_PROFILE_ID, QWEN_8B_PROFILE_ID]
        .into_iter()
        .find(|profile_id| {
            embedding_config_for_profile(profile_id, config)
                .is_ok_and(|profile| same_profile_identity(config, &profile))
        })
        .unwrap_or("legacy-custom")
        .to_string()
}

fn same_profile_identity(left: &EmbeddingConfig, right: &EmbeddingConfig) -> bool {
    left.model_path.is_none()
        && right.model_path.is_none()
        && left.repo == right.repo
        && left.filename == right.filename
        && left.revision == right.revision
        && left.pooling == right.pooling
        && left.attention == right.attention
        && left.query_template == right.query_template
        && left.passage_template == right.passage_template
        && left.add_bos == right.add_bos
        && left.append_eos == right.append_eos
        && left.normalize == right.normalize
        && left.dimension == right.dimension
        && left.context_size == right.context_size
}

pub(crate) fn switch_status_from_disk(
    config: &MemoryConfig,
    switch_id: &str,
) -> anyhow::Result<ModelSwitchStatusResponse> {
    let store = ModelSwitchStore::load(&config.model_switch_path(), config.project_id())?;
    let job = store
        .find(switch_id)
        .ok_or_else(|| anyhow::anyhow!("model switch not found: {switch_id}"))?;
    let active = ActiveEmbedding::load(&config.active_embedding_path(), config.project_id())?;
    let (active_profile_id, active_generation_id) = active.map_or_else(
        || {
            (
                configured_profile_id(config.embedding()),
                "legacy".to_string(),
            )
        },
        |active| (active.profile_id, active.generation_id),
    );
    Ok(ModelSwitchStatusResponse {
        switch_id: job.switch_id.clone(),
        state: switch_phase_name(job.phase).to_string(),
        active_profile_id,
        target_profile_id: job.target_profile_id.clone(),
        active_generation_id,
        target_generation_id: Some(job.target_generation_id.clone()),
        completed_records: job.completed_records,
        total_records: job.total_records,
        fraction: if job.total_records == 0 {
            f64::from(job.phase == SwitchPhase::Succeeded)
        } else {
            job.completed_records as f64 / job.total_records as f64
        },
        cancel_requested: job.cancel_requested,
        dense_search_available: true,
        created_at_ms: job.created_at_ms,
        updated_at_ms: job.updated_at_ms,
        completed_at_ms: job.completed_at_ms,
        error: job.error_code.as_ref().map(|code| ModelProfileReason {
            code: code.clone(),
            message: job.error_message.clone().unwrap_or_default(),
        }),
    })
}

pub(crate) fn switch_exists(config: &MemoryConfig, switch_id: &str) -> anyhow::Result<bool> {
    Ok(
        ModelSwitchStore::load(&config.model_switch_path(), config.project_id())?
            .find(switch_id)
            .is_some(),
    )
}

pub(crate) fn cancel_switch_from_disk(
    config: &MemoryConfig,
    switch_id: &str,
) -> anyhow::Result<ModelSwitchCancelResponse> {
    let mut store = ModelSwitchStore::load(&config.model_switch_path(), config.project_id())?;
    let Some(job) = store.current.as_mut() else {
        return Ok(ModelSwitchCancelResponse {
            switch_id: switch_id.to_string(),
            outcome: "not_found".to_string(),
        });
    };
    if job.switch_id != switch_id {
        return Ok(ModelSwitchCancelResponse {
            switch_id: switch_id.to_string(),
            outcome: "not_found".to_string(),
        });
    }
    let outcome = match job.phase {
        SwitchPhase::Committing => "already_committing",
        SwitchPhase::Succeeded => "already_committed",
        SwitchPhase::Cancelled | SwitchPhase::Failed => "already_terminal",
        phase if phase.can_cancel() => {
            job.cancel_requested = true;
            job.phase = SwitchPhase::CancelRequested;
            job.updated_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| anyhow::anyhow!("system clock is before Unix epoch"))?
                .as_millis()
                .try_into()?;
            "cancel_requested"
        }
        _ => "already_terminal",
    };
    store.save(&config.model_switch_path())?;
    Ok(ModelSwitchCancelResponse {
        switch_id: switch_id.to_string(),
        outcome: outcome.to_string(),
    })
}

fn switch_phase_name(phase: SwitchPhase) -> &'static str {
    match phase {
        SwitchPhase::Queued => "queued",
        SwitchPhase::Validating => "validating",
        SwitchPhase::Downloading => "downloading",
        SwitchPhase::Preparing => "preparing",
        SwitchPhase::Reindexing => "reindexing",
        SwitchPhase::Verifying => "verifying",
        SwitchPhase::Committing => "committing",
        SwitchPhase::Succeeded => "succeeded",
        SwitchPhase::CancelRequested => "cancel_requested",
        SwitchPhase::Cancelled => "cancelled",
        SwitchPhase::Failed => "failed",
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

fn qwen_06b_profile(installed: bool) -> ModelProfile {
    gguf_preview(
        "qwen3-text-0.6b-q8",
        "Qwen3 Embedding 0.6B Q8_0",
        "Qwen/Qwen3-Embedding-0.6B-GGUF",
        "Qwen3-Embedding-0.6B-Q8_0.gguf",
        1024,
        639_150_592,
        1_500_000_000,
        installed,
    )
}

fn qwen_8b_profile(installed: bool) -> ModelProfile {
    gguf_preview(
        "qwen3-text-8b-q4",
        "Qwen3 Embedding 8B Q4_K_M",
        "Qwen/Qwen3-Embedding-8B-GGUF",
        "Qwen3-Embedding-8B-Q4_K_M.gguf",
        4096,
        4_676_804_928,
        11_000_000_000,
        installed,
    )
}

#[allow(clippy::too_many_arguments)]
fn gguf_preview(
    profile_id: &str,
    display_name: &str,
    repository: &str,
    filename: &str,
    dimension: usize,
    download_bytes: u64,
    resident_bytes: u64,
    installed: bool,
) -> ModelProfile {
    ModelProfile {
        profile_id: profile_id.to_string(),
        display_name: display_name.to_string(),
        description:
            "Immutable GGUF text embedding profile available for project generation migration."
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
        support_level: "stable".to_string(),
        selectable: true,
        default_for_new_projects: false,
        recommended: false,
        installed,
        platform_supported: true,
        runtime_available: true,
        artifact_locked: true,
        estimated_download_bytes: Some(download_bytes),
        estimated_resident_bytes: Some(resident_bytes),
        unavailable_reason: None,
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

fn directory_size(path: &std::path::Path) -> anyhow::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

pub(crate) fn embedding_config_for_profile(
    profile_id: &str,
    runtime: &EmbeddingConfig,
) -> anyhow::Result<EmbeddingConfig> {
    let (repo, filename, revision, dimension) = match profile_id {
        CURRENT_PROFILE_ID => (
            CURRENT_PROFILE_REPOSITORY,
            CURRENT_PROFILE_FILENAME,
            CURRENT_PROFILE_REVISION,
            None,
        ),
        QWEN_06B_PROFILE_ID => (
            "Qwen/Qwen3-Embedding-0.6B-GGUF",
            "Qwen3-Embedding-0.6B-Q8_0.gguf",
            "370f27d7550e0def9b39c1f16d3fbaa13aa67728",
            Some(1024),
        ),
        QWEN_8B_PROFILE_ID => (
            "Qwen/Qwen3-Embedding-8B-GGUF",
            "Qwen3-Embedding-8B-Q4_K_M.gguf",
            "69d0e58a13e463cd99a9b83e3f5fee7c10265fab",
            Some(4096),
        ),
        "legacy-custom" => return Ok(runtime.clone()),
        _ => anyhow::bail!("PROFILE_NOT_FOUND: unknown embedding profile {profile_id}"),
    };
    Ok(EmbeddingConfig {
        model_path: None,
        repo: repo.to_string(),
        filename: filename.to_string(),
        revision: revision.to_string(),
        pooling: "last".to_string(),
        attention: "causal".to_string(),
        query_template: EmbeddingConfig::default().query_template,
        passage_template: "{text}".to_string(),
        add_bos: true,
        append_eos: true,
        normalize: true,
        dimension,
        context_size: 8_192,
        threads: runtime.threads,
        gpu_layers: runtime.gpu_layers,
    })
}

pub(crate) fn artifact_sha256(profile_id: &str) -> Option<&'static str> {
    match profile_id {
        CURRENT_PROFILE_ID => {
            Some("2b0cf8f17b4c723c27303015383c27ec4bf2d8314bb677d05e920dd70bb0f16b")
        }
        QWEN_06B_PROFILE_ID => {
            Some("06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439")
        }
        QWEN_8B_PROFILE_ID => {
            Some("3fcd3febec8b3fd64435204db75bf0dd73b91e8d0661e0331acfe7e7c3120b85")
        }
        _ => None,
    }
}

fn profile_installed(model_cache: &std::path::Path, profile_id: &str) -> bool {
    let Ok(config) = embedding_config_for_profile(profile_id, &EmbeddingConfig::default()) else {
        return false;
    };
    model_cache
        .join(format!("models--{}", config.repo.replace('/', "--")))
        .join("snapshots")
        .join(config.revision)
        .join(config.filename)
        .is_file()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        CURRENT_PROFILE_ID, ModelSwitchRequest, configured_profile_id,
        embedding_config_for_profile, preflight, profiles,
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
            3
        );
        assert!(response.profiles[0].default_for_new_projects);
        assert_eq!(response.catalog_digest.len(), 64);
        assert!(response.profiles[0].artifact_locked);
        assert!(response.profiles[0].artifact_sha256.is_some());
        assert!(!response.profiles[0].installed);
    }

    #[test]
    fn configured_profile_recognizes_each_selectable_catalog_profile() {
        for profile_id in [CURRENT_PROFILE_ID, "qwen3-text-0.6b-q8", "qwen3-text-8b-q4"] {
            let config = embedding_config_for_profile(profile_id, &EmbeddingConfig::default())
                .expect("catalog profile config");
            assert_eq!(configured_profile_id(&config), profile_id);
        }
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
                expected_active_generation_id: None,
                allow_dense_downtime: false,
                dry_run: true,
                force_rebuild: false,
                retain_previous: true,
                target_generation_id: None,
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
