//! Whisper model resolution: map a [`WhisperModel`] variant to on-disk ONNX paths,
//! downloading from Hugging Face Hub on first use.
//!
//! Model files remain in hf-hub's content-addressed snapshot cache. By default,
//! hf-hub follows the standard Hugging Face environment-variable and platform
//! cache conventions. An explicit `cache_dir` is treated as an alternate
//! Hugging Face cache root; Xberg never stages a second copy.
//!
//! # HF repos
//!
//! Tiny, Base, and Small are fetched from `onnx-community/whisper-{size}`;
//! Medium and LargeV3 from `Xenova/whisper-{size}` (onnx-community does not
//! publish ONNX exports for those two sizes). All use the same file layout.
//!
//! # Decoder layout
//!
//! Small, Medium, and LargeV3 export a single merged decoder file
//! (`onnx/decoder_model_merged.onnx`) used for both the initial and KV-cache
//! passes. LargeV3 additionally carries its weights in an external
//! `onnx/decoder_model_merged.onnx_data` shard (its decoder exceeds the 2 GiB
//! protobuf limit); Small and Medium are self-contained. Tiny and Base ship
//! separate `decoder_model.onnx` / `decoder_with_past_model.onnx` files.

#[cfg(feature = "transcription")]
use std::path::{Path, PathBuf};

#[cfg(feature = "transcription")]
use crate::core::config::transcription::WhisperModel;

/// On-disk paths for all files needed to load a Whisper model in an ORT session.
#[cfg(feature = "transcription")]
#[cfg_attr(alef, alef(skip))]
#[derive(Debug, Clone)]
pub struct WhisperModelPaths {
    /// Encoder ONNX model: `onnx/encoder_model.onnx`.
    pub encoder: PathBuf,
    /// Decoder ONNX model (without KV-cache past): `onnx/decoder_model.onnx`.
    /// For sharded variants (Small+) this is the merged decoder.
    pub decoder: PathBuf,
    /// Decoder ONNX model with KV-cache past: `onnx/decoder_with_past_model.onnx`.
    /// For sharded variants (Small+) this points to the same merged decoder as `decoder`.
    pub decoder_with_past: PathBuf,
    /// `tokenizer.json` — vocabulary + BPE rules.
    pub tokenizer: PathBuf,
    /// `config.json` — model hyper-parameters.
    pub config: PathBuf,
    /// Number of mel filter banks expected by this model's audio pre-processor.
    /// 80 for Tiny / Base / Small / Medium; 128 for LargeV3.
    pub n_mels: u32,
}

/// Errors that can occur while resolving Whisper model paths.
#[cfg(feature = "transcription")]
#[derive(Debug, thiserror::Error)]
#[cfg_attr(alef, alef(skip))]
pub enum WhisperModelError {
    /// The model is not cached locally and `allow_network` is `false`.
    #[error("network access disabled and model not cached: {0}")]
    ModelMissing(String),

    /// A file download from Hugging Face Hub failed.
    #[error("hf-hub download failed: {0}")]
    Download(String),

    /// An I/O error occurred while loading a resolved model artifact.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The resolved cache directory could not be determined or created.
    #[error("cache directory unavailable: {0}")]
    Cache(String),

    /// Hash verification was requested, but the resolver has no checked-in
    /// checksum manifest for the pinned Whisper snapshots.
    #[error("hash verification is unavailable because Whisper checksums are not bundled")]
    HashVerificationUnavailable,
}

/// Map a [`WhisperModel`] to its Hugging Face Hub repository identifier.
#[cfg(feature = "transcription")]
pub(crate) fn hf_repo(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::Tiny => "onnx-community/whisper-tiny",
        WhisperModel::Base => "onnx-community/whisper-base",
        WhisperModel::Small => "onnx-community/whisper-small",
        WhisperModel::Medium => "Xenova/whisper-medium",
        WhisperModel::LargeV3 => "Xenova/whisper-large-v3",
    }
}

/// Immutable Hugging Face revision for each supported Whisper export.
#[cfg(feature = "transcription")]
pub(crate) fn hf_revision(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::Tiny => "ff4177021cc41f7db950912b73ea4fdf7d01d8e7",
        WhisperModel::Base => "1846881b6b3a3024392c1eea3ad983695bc23925",
        WhisperModel::Small => "36050c46d777d46dc4b5f43f6d90574fc38f8732",
        WhisperModel::Medium => "8c5b90880ab9f79487ab33613413431bf661d595",
        WhisperModel::LargeV3 => "67bf02d92b7754a1ff82a7f8545f8b8c378b2ef0",
    }
}

/// Number of mel filter banks the audio pre-processor produces for `model`.
///
/// 80 for every model except LargeV3, which uses 128.
#[cfg(feature = "transcription")]
pub(crate) fn n_mels(model: WhisperModel) -> u32 {
    match model {
        WhisperModel::LargeV3 => 128,
        _ => 80,
    }
}

/// Returns `true` when the model ships its decoder as a single merged file
/// (`decoder_model_merged.onnx`), used for both the initial and KV-cache
/// passes (Small, Medium, LargeV3).
///
/// Tiny and Base ship separate `decoder_model.onnx` /
/// `decoder_with_past_model.onnx` files instead.
#[cfg(feature = "transcription")]
fn is_sharded(model: WhisperModel) -> bool {
    matches!(
        model,
        WhisperModel::Small | WhisperModel::Medium | WhisperModel::LargeV3
    )
}

/// Returns `true` when the merged decoder carries its weights in a separate
/// external `.onnx_data` shard. Only LargeV3's decoder exceeds the 2 GB
/// protobuf limit; Small and Medium ship a self-contained merged decoder.
#[cfg(feature = "transcription")]
fn has_external_data_shard(model: WhisperModel) -> bool {
    matches!(model, WhisperModel::LargeV3)
}

/// Remote paths (relative to the repo root on HF Hub) for the files that
/// must be downloaded for `model`.
///
/// Returns `(remote_path, logical_name)` pairs. The logical name only maps the
/// resolved snapshot path into [`WhisperModelPaths`]; no renamed cache copy is
/// created.
#[cfg(feature = "transcription")]
fn model_files(model: WhisperModel) -> Vec<(&'static str, &'static str)> {
    if is_sharded(model) {
        let mut files = vec![
            ("onnx/encoder_model.onnx", "encoder.onnx"),
            ("onnx/decoder_model_merged.onnx", "decoder.onnx"),
        ];
        if has_external_data_shard(model) {
            files.push(("onnx/decoder_model_merged.onnx_data", "decoder.onnx_data"));
        }
        files.push(("tokenizer.json", "tokenizer.json"));
        files.push(("config.json", "config.json"));
        files
    } else {
        vec![
            ("onnx/encoder_model.onnx", "encoder.onnx"),
            ("onnx/decoder_model.onnx", "decoder.onnx"),
            ("onnx/decoder_with_past_model.onnx", "decoder_with_past.onnx"),
            ("tokenizer.json", "tokenizer.json"),
            ("config.json", "config.json"),
        ]
    }
}

/// Resolve a Whisper model to on-disk ONNX paths, downloading from HF Hub if needed.
///
/// # Behaviour
///
/// 1. Resolve every required file at the model's immutable revision through
///    hf-hub's snapshot cache.
/// 2. If `allow_network` is `false`, inspect only existing cache entries.
/// 3. If any required file is absent in cache-only mode, return
///    [`WhisperModelError::ModelMissing`].
/// 4. Return the hf-hub snapshot paths directly without copying model bytes.
///
/// # Errors
///
/// Returns [`WhisperModelError`] on I/O failures, download failures, or when the
/// model is unavailable and `allow_network` is `false`.
#[cfg(feature = "transcription")]
#[cfg_attr(alef, alef(skip))]
pub fn ensure_whisper_model(
    model: WhisperModel,
    cache_dir: Option<&Path>,
    allow_network: bool,
    verify_hash: bool,
) -> Result<WhisperModelPaths, WhisperModelError> {
    if verify_hash {
        return Err(WhisperModelError::HashVerificationUnavailable);
    }

    let revision = hf_revision(model);
    let mut resolved = std::collections::HashMap::new();
    for (remote_path, local_name) in model_files(model) {
        let path = if allow_network {
            crate::model_download::hf_resolve_file(hf_repo(model), remote_path, Some(revision), cache_dir, None)
                .map_err(WhisperModelError::Download)?
        } else {
            crate::model_download::hf_cached_file(hf_repo(model), remote_path, Some(revision), cache_dir)
                .map_err(WhisperModelError::Download)?
                .ok_or_else(|| WhisperModelError::ModelMissing(format!("{}@{revision}", hf_repo(model))))?
        };
        resolved.insert(local_name, path);
    }

    build_paths(model, resolved)
}

/// Construct [`WhisperModelPaths`] from resolved hf-hub snapshot entries.
///
/// For sharded models (Small, Medium, LargeV3) both `decoder` and
/// `decoder_with_past` point at the merged decoder file.
#[cfg(feature = "transcription")]
fn build_paths(
    model: WhisperModel,
    mut resolved: std::collections::HashMap<&'static str, PathBuf>,
) -> Result<WhisperModelPaths, WhisperModelError> {
    let mut take = |name: &str| {
        resolved
            .remove(name)
            .ok_or_else(|| WhisperModelError::Cache(format!("resolved Whisper snapshot is missing {name}")))
    };
    let encoder = take("encoder.onnx")?;
    let tokenizer = take("tokenizer.json")?;
    let config = take("config.json")?;

    let (decoder, decoder_with_past) = if is_sharded(model) {
        let merged = take("decoder.onnx")?;
        (merged.clone(), merged)
    } else {
        (take("decoder.onnx")?, take("decoder_with_past.onnx")?)
    };

    Ok(WhisperModelPaths {
        encoder,
        decoder,
        decoder_with_past,
        tokenizer,
        config,
        n_mels: n_mels(model),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "transcription")]
    use crate::core::config::transcription::WhisperModel;

    #[cfg(feature = "transcription")]
    fn resolved_fixture(model: WhisperModel, directory: &Path) -> std::collections::HashMap<&'static str, PathBuf> {
        model_files(model)
            .into_iter()
            .map(|(_, local_name)| (local_name, directory.join(local_name)))
            .collect()
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn hf_repo_points_at_published_onnx_exports() {
        assert_eq!(hf_repo(WhisperModel::Tiny), "onnx-community/whisper-tiny");
        assert_eq!(hf_repo(WhisperModel::Base), "onnx-community/whisper-base");
        assert_eq!(hf_repo(WhisperModel::Small), "onnx-community/whisper-small");
        assert_eq!(hf_repo(WhisperModel::Medium), "Xenova/whisper-medium");
        assert_eq!(hf_repo(WhisperModel::LargeV3), "Xenova/whisper-large-v3");
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn only_large_v3_uses_external_data_shard() {
        assert!(!has_external_data_shard(WhisperModel::Small));
        assert!(!has_external_data_shard(WhisperModel::Medium));
        assert!(has_external_data_shard(WhisperModel::LargeV3));
        assert!(
            !model_files(WhisperModel::Small)
                .iter()
                .any(|(remote, _)| remote.ends_with(".onnx_data"))
        );
        assert!(
            model_files(WhisperModel::LargeV3)
                .iter()
                .any(|(remote, _)| remote.ends_with(".onnx_data"))
        );
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn n_mels_is_128_only_for_large_v3() {
        for model in [
            WhisperModel::Tiny,
            WhisperModel::Base,
            WhisperModel::Small,
            WhisperModel::Medium,
        ] {
            assert_eq!(n_mels(model), 80, "{model:?} should have 80 mels");
        }
        assert_eq!(n_mels(WhisperModel::LargeV3), 128);
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn ensure_model_returns_missing_when_network_disabled_and_uncached() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = ensure_whisper_model(WhisperModel::Tiny, Some(tmp.path()), false, false);
        assert!(
            matches!(result, Err(WhisperModelError::ModelMissing(_))),
            "expected ModelMissing, got: {result:?}",
        );
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn verify_hash_requests_fail_fast() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = ensure_whisper_model(WhisperModel::Tiny, Some(tmp.path()), false, true);
        assert!(
            matches!(result, Err(WhisperModelError::HashVerificationUnavailable)),
            "expected HashVerificationUnavailable, got: {result:?}",
        );
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn sharded_models_use_merged_decoder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = build_paths(WhisperModel::Small, resolved_fixture(WhisperModel::Small, tmp.path()))
            .expect("fixture contains all paths");

        assert_eq!(
            paths.decoder, paths.decoder_with_past,
            "sharded model: decoder and decoder_with_past must point at the merged file",
        );
        assert_eq!(paths.n_mels, 80);
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn non_sharded_models_have_distinct_decoder_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = build_paths(WhisperModel::Tiny, resolved_fixture(WhisperModel::Tiny, tmp.path()))
            .expect("fixture contains all paths");

        assert_ne!(
            paths.decoder, paths.decoder_with_past,
            "non-sharded model: decoder and decoder_with_past must be distinct files",
        );
        assert_eq!(paths.n_mels, 80);
    }

    #[cfg(feature = "transcription")]
    #[test]
    fn large_v3_uses_128_mels_from_cached_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = build_paths(
            WhisperModel::LargeV3,
            resolved_fixture(WhisperModel::LargeV3, tmp.path()),
        )
        .expect("fixture contains all paths");

        assert_eq!(paths.n_mels, 128);
    }
}
