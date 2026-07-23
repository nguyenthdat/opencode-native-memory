//! Configurable local embedding inference through llama.cpp and GGUF models.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use hf_hub::{HFClient, split_id};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaAttentionType, LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use self_cell::self_cell;

use crate::EmbeddingConfig;

self_cell!(
    struct ModelSession {
        owner: LlamaModel,

        #[covariant]
        dependent: LlamaContext,
    }
);

/// Backend-independent embedding operations consumed by the memory engine.
pub(crate) trait Embedder {
    fn model_id(&self) -> &str;
    fn dimension(&self) -> usize;
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>>;
    fn embed_passage(&mut self, text: &str) -> Result<Vec<f32>>;
}

pub(crate) struct LlamaCppEmbedder {
    // Drop the borrowing model/context session before freeing the llama backend.
    session: ModelSession,
    _backend: LlamaBackend,
    model_id: String,
    dimension: usize,
    context_size: usize,
    query_template: String,
    passage_template: String,
    add_bos: AddBos,
    append_eos: bool,
    normalize: bool,
}

impl LlamaCppEmbedder {
    pub(crate) fn load(config: &EmbeddingConfig, cache_dir: &Path) -> Result<Self> {
        let model_path = resolve_model_path(config, cache_dir)?;
        let backend = LlamaBackend::init().context("cannot initialize llama.cpp backend")?;
        let gpu_layers = config.gpu_layers.unwrap_or_else(|| {
            if backend.supports_gpu_offload() {
                1_000
            } else {
                0
            }
        });
        let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params).with_context(
            || format!("cannot load GGUF embedding model {}", model_path.display()),
        )?;

        let native_dimension =
            usize::try_from(model.n_embd()).context("GGUF embedding dimension is invalid")?;
        let dimension = config.dimension.unwrap_or(native_dimension);
        ensure!(
            dimension <= native_dimension,
            "requested embedding dimension {dimension} exceeds model dimension {native_dimension}"
        );
        let context_size = config.context_size.min(model.n_ctx_train());
        ensure!(
            context_size > 0,
            "GGUF model reports an empty context window"
        );
        let pooling = parse_pooling(&config.pooling)?;
        ensure!(
            pooling != LlamaPoolingType::None && pooling != LlamaPoolingType::Rank,
            "memory storage requires one dense vector; pooling must be last, mean, cls, or unspecified"
        );
        let attention = parse_attention(&config.attention)?;
        let threads = config.threads.unwrap_or_else(default_threads);
        ensure!(
            threads > 0,
            "embedding thread count must be greater than zero"
        );
        let context_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_size))
            .with_n_batch(context_size)
            .with_n_ubatch(context_size)
            .with_n_seq_max(1)
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_embeddings(true)
            .with_pooling_type(pooling)
            .with_attention_type(attention)
            .with_kv_unified(true);
        let session = ModelSession::try_new(model, |model| {
            model
                .new_context(&backend, context_params)
                .context("cannot create llama.cpp embedding context")
        })?;
        let model_id = if config.model_path.is_some() {
            format!("local:{}", model_path.display())
        } else {
            format!("hf:{}@{}/{}", config.repo, config.revision, config.filename)
        };

        Ok(Self {
            session,
            _backend: backend,
            model_id,
            dimension,
            context_size: usize::try_from(context_size)?,
            query_template: config.query_template.clone(),
            passage_template: config.passage_template.clone(),
            add_bos: if config.add_bos {
                AddBos::Always
            } else {
                AddBos::Never
            },
            append_eos: config.append_eos,
            normalize: config.normalize,
        })
    }

    fn embed(&mut self, text: &str, template: &str) -> Result<Vec<f32>> {
        let prompt = template.replace("{text}", text);
        let add_bos = self.add_bos;
        let append_eos = self.append_eos;
        let context_size = self.context_size;
        let dimension = self.dimension;
        let normalize = self.normalize;

        self.session.with_dependent_mut(|model, context| {
            let mut tokens = model
                .str_to_token(&prompt, add_bos)
                .context("cannot tokenize embedding input")?;
            if append_eos {
                let eos = model.token_eos();
                let separator = model.token_sep();
                if !tokens
                    .last()
                    .is_some_and(|token| *token == eos || *token == separator)
                {
                    tokens.push(eos);
                }
            }
            ensure!(!tokens.is_empty(), "embedding input produced no tokens");
            ensure!(
                tokens.len() <= context_size,
                "embedding input has {} tokens, exceeding configured context size {context_size}",
                tokens.len()
            );

            let mut batch = LlamaBatch::new(tokens.len(), 1);
            batch
                .add_sequence(&tokens, 0, true)
                .context("cannot create llama.cpp embedding batch")?;
            context.clear_kv_cache();
            context
                .decode(&mut batch)
                .context("local GGUF embedding inference failed")?;
            let raw = context
                .embeddings_seq_ith(0)
                .context("GGUF model did not return a pooled embedding")?;
            ensure!(
                dimension <= raw.len(),
                "configured embedding dimension {dimension} exceeds returned dimension {}",
                raw.len()
            );
            let mut output = raw[..dimension].to_vec();
            if normalize {
                l2_normalize(&mut output)?;
            }
            Ok(output)
        })
    }
}

impl Embedder for LlamaCppEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let template = self.query_template.clone();
        self.embed(text, &template)
    }

    fn embed_passage(&mut self, text: &str) -> Result<Vec<f32>> {
        let template = self.passage_template.clone();
        self.embed(text, &template)
    }
}

fn resolve_model_path(config: &EmbeddingConfig, cache_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = &config.model_path {
        ensure!(
            path.is_file(),
            "GGUF model does not exist: {}",
            path.display()
        );
        return path
            .canonicalize()
            .with_context(|| format!("cannot resolve GGUF model path {}", path.display()));
    }
    ensure!(
        config.filename.to_ascii_lowercase().ends_with(".gguf"),
        "Hugging Face embedding model file must be GGUF"
    );
    let (owner, name) = split_id(&config.repo);
    ensure!(
        !owner.is_empty() && !name.is_empty(),
        "embedding model repo must use owner/name format"
    );
    let client = HFClient::builder()
        .cache_dir(cache_dir)
        .build_sync()
        .context("cannot initialize Hugging Face client")?;
    client
        .model(owner, name)
        .download_file()
        .filename(&config.filename)
        .revision(&config.revision)
        .send()
        .with_context(|| {
            format!(
                "cannot download {}/{} from Hugging Face revision {}",
                config.repo, config.filename, config.revision
            )
        })
}

fn parse_pooling(value: &str) -> Result<LlamaPoolingType> {
    match value.to_ascii_lowercase().as_str() {
        "last" => Ok(LlamaPoolingType::Last),
        "mean" => Ok(LlamaPoolingType::Mean),
        "cls" => Ok(LlamaPoolingType::Cls),
        "unspecified" | "model" => Ok(LlamaPoolingType::Unspecified),
        "none" => Ok(LlamaPoolingType::None),
        "rank" => Ok(LlamaPoolingType::Rank),
        _ => bail!("unknown embedding pooling {value}; expected last, mean, cls, or model"),
    }
}

fn parse_attention(value: &str) -> Result<LlamaAttentionType> {
    match value.to_ascii_lowercase().as_str() {
        "causal" => Ok(LlamaAttentionType::Causal),
        "non_causal" | "non-causal" => Ok(LlamaAttentionType::NonCausal),
        "unspecified" | "model" => Ok(LlamaAttentionType::Unspecified),
        _ => bail!("unknown embedding attention {value}; expected causal, non_causal, or model"),
    }
}

fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .ok()
        .and_then(|threads| i32::try_from(threads.get()).ok())
        .unwrap_or(1)
}

fn l2_normalize(values: &mut [f32]) -> Result<()> {
    let norm = values
        .iter()
        .map(|value| {
            let value = f64::from(*value);
            value * value
        })
        .sum::<f64>()
        .sqrt();
    ensure!(
        norm.is_finite() && norm > f64::EPSILON,
        "embedding model returned a zero or invalid vector"
    );
    for value in values {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{l2_normalize, parse_attention, parse_pooling};
    use llama_cpp_2::context::params::{LlamaAttentionType, LlamaPoolingType};

    #[test]
    fn parses_model_profile_values() {
        assert_eq!(parse_pooling("last").unwrap(), LlamaPoolingType::Last);
        assert_eq!(
            parse_attention("non-causal").unwrap(),
            LlamaAttentionType::NonCausal
        );
        assert!(parse_pooling("average").is_err());
    }

    #[test]
    fn normalizes_embedding_after_dimension_truncation() {
        let mut values = vec![3.0, 4.0];
        l2_normalize(&mut values).unwrap();
        assert!((values[0] - 0.6).abs() < f32::EPSILON);
        assert!((values[1] - 0.8).abs() < f32::EPSILON);
    }
}
