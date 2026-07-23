use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

const DATA_SUBDIR: &str = "opencode/memory";
const MODEL_CACHE_SUBDIR: &str = "opencode/memory/models";
const DEFAULT_MODEL_REPO: &str = "Qwen/Qwen3-Embedding-4B-GGUF";
const DEFAULT_MODEL_FILE: &str = "Qwen3-Embedding-4B-Q4_K_M.gguf";
const DEFAULT_MODEL_REVISION: &str = "f4602530db1d980e16da9d7d3a70294cf5c190be";
const DEFAULT_QUERY_TEMPLATE: &str = "Instruct: Given a code search query, retrieve relevant passages that answer the query\nQuery:{text}";

/// Runtime configuration for a llama.cpp-compatible GGUF embedding model.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EmbeddingConfig {
    pub(crate) model_path: Option<PathBuf>,
    pub(crate) repo: String,
    pub(crate) filename: String,
    pub(crate) revision: String,
    pub(crate) pooling: String,
    pub(crate) attention: String,
    pub(crate) query_template: String,
    pub(crate) passage_template: String,
    pub(crate) add_bos: bool,
    pub(crate) append_eos: bool,
    pub(crate) normalize: bool,
    pub(crate) dimension: Option<usize>,
    pub(crate) context_size: u32,
    pub(crate) threads: Option<i32>,
    pub(crate) gpu_layers: Option<u32>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_path: None,
            repo: DEFAULT_MODEL_REPO.to_string(),
            filename: DEFAULT_MODEL_FILE.to_string(),
            revision: DEFAULT_MODEL_REVISION.to_string(),
            pooling: "last".to_string(),
            attention: "causal".to_string(),
            query_template: DEFAULT_QUERY_TEMPLATE.to_string(),
            passage_template: "{text}".to_string(),
            add_bos: true,
            append_eos: true,
            normalize: true,
            dimension: None,
            context_size: 8_192,
            threads: None,
            gpu_layers: None,
        }
    }
}

impl EmbeddingConfig {
    fn discover() -> Result<Self> {
        let defaults = Self::default();
        let config = Self {
            model_path: env_path("OPENCODE_MEMORY_EMBEDDING_MODEL_PATH"),
            repo: env_string("OPENCODE_MEMORY_EMBEDDING_MODEL_REPO").unwrap_or(defaults.repo),
            filename: env_string("OPENCODE_MEMORY_EMBEDDING_MODEL_FILE")
                .unwrap_or(defaults.filename),
            revision: env_string("OPENCODE_MEMORY_EMBEDDING_MODEL_REVISION")
                .unwrap_or(defaults.revision),
            pooling: env_string("OPENCODE_MEMORY_EMBEDDING_POOLING").unwrap_or(defaults.pooling),
            attention: env_string("OPENCODE_MEMORY_EMBEDDING_ATTENTION")
                .unwrap_or(defaults.attention),
            query_template: env_string("OPENCODE_MEMORY_EMBEDDING_QUERY_TEMPLATE")
                .unwrap_or(defaults.query_template),
            passage_template: env_string("OPENCODE_MEMORY_EMBEDDING_PASSAGE_TEMPLATE")
                .unwrap_or(defaults.passage_template),
            add_bos: env_bool("OPENCODE_MEMORY_EMBEDDING_ADD_BOS")?.unwrap_or(defaults.add_bos),
            append_eos: env_bool("OPENCODE_MEMORY_EMBEDDING_APPEND_EOS")?
                .unwrap_or(defaults.append_eos),
            normalize: env_bool("OPENCODE_MEMORY_EMBEDDING_NORMALIZE")?
                .unwrap_or(defaults.normalize),
            dimension: env_parse("OPENCODE_MEMORY_EMBEDDING_DIMENSION")?,
            context_size: env_parse("OPENCODE_MEMORY_EMBEDDING_CONTEXT_SIZE")?
                .unwrap_or(defaults.context_size),
            threads: env_parse("OPENCODE_MEMORY_EMBEDDING_THREADS")?,
            gpu_layers: env_parse("OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS")?,
        };
        anyhow::ensure!(
            config.query_template.contains("{text}") && config.passage_template.contains("{text}"),
            "embedding query and passage templates must contain {{text}}"
        );
        anyhow::ensure!(
            config.context_size > 0,
            "embedding context size must be greater than zero"
        );
        if let Some(dimension) = config.dimension {
            anyhow::ensure!(
                dimension > 0,
                "embedding dimension must be greater than zero"
            );
        }
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    project_root: PathBuf,
    project_id: String,
    data_root: PathBuf,
    model_cache: PathBuf,
    embedding: EmbeddingConfig,
}

impl MemoryConfig {
    /// Discover project, storage, and model-cache paths from the environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the current working directory cannot be determined.
    pub fn discover() -> Result<Self> {
        let project_root = match env::var_os("OPENCODE_MEMORY_PROJECT_ROOT") {
            Some(value) => resolve_project_root(PathBuf::from(value), false),
            None => resolve_project_root(
                env::current_dir().context("cannot determine the current project directory")?,
                true,
            ),
        };

        let embedding = EmbeddingConfig::discover()?;
        let data_home = default_data_home();
        let data_root =
            env_path("OPENCODE_MEMORY_DATA_DIR").unwrap_or_else(|| data_home.join(DATA_SUBDIR));
        let model_cache = resolve_model_cache(
            env_path("OPENCODE_MEMORY_MODEL_CACHE"),
            &data_home,
            &embedding.revision,
        );

        Ok(Self::new(project_root, data_root, model_cache).with_embedding(embedding))
    }

    #[must_use]
    pub fn new(project_root: PathBuf, data_root: PathBuf, model_cache: PathBuf) -> Self {
        let canonical = project_root.canonicalize().unwrap_or(project_root);
        let project_id = hash_hex(canonical.to_string_lossy().as_bytes());
        Self {
            project_root: canonical,
            project_id,
            data_root,
            model_cache,
            embedding: EmbeddingConfig::default(),
        }
    }

    /// Override the embedding model configuration.
    #[must_use]
    pub fn with_embedding(mut self, embedding: EmbeddingConfig) -> Self {
        self.embedding = embedding;
        self
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    #[must_use]
    pub fn model_cache(&self) -> &Path {
        &self.model_cache
    }

    #[must_use]
    pub(crate) fn embedding(&self) -> &EmbeddingConfig {
        &self.embedding
    }

    #[must_use]
    pub fn project_data_dir(&self) -> PathBuf {
        self.data_root.join("projects").join(&self.project_id)
    }

    #[must_use]
    pub fn collection_dir(&self) -> PathBuf {
        self.project_data_dir().join("zvec")
    }

    #[must_use]
    pub fn state_path(&self) -> PathBuf {
        self.project_data_dir().join("state.json")
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_string(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn env_parse<T>(name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    env_string(name)
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| anyhow::anyhow!("invalid {name}: {value}: {error}"))
        })
        .transpose()
}

fn env_bool(name: &str) -> Result<Option<bool>> {
    let Some(value) = env_string(name) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => anyhow::bail!("invalid {name}: expected true or false, received {value}"),
    }
}

fn default_data_home() -> PathBuf {
    if let Some(path) = env_path("XDG_DATA_HOME") {
        return path;
    }
    home_dir().join(".local/share")
}

fn default_model_cache(data_home: &Path, revision: &str) -> PathBuf {
    data_home
        .join(MODEL_CACHE_SUBDIR)
        .join(revision_cache_component(revision))
}

fn resolve_model_cache(
    override_path: Option<PathBuf>,
    data_home: &Path,
    revision: &str,
) -> PathBuf {
    override_path.unwrap_or_else(|| default_model_cache(data_home, revision))
}

fn revision_cache_component(revision: &str) -> String {
    if !revision.is_empty()
        && revision != "."
        && revision != ".."
        && revision.len() <= 128
        && revision.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        revision.to_string()
    } else {
        format!("revision-{}", &hash_hex(revision.as_bytes())[..16])
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn discover_project_root(start: &Path) -> PathBuf {
    for candidate in start.ancestors() {
        if candidate.join(".git").exists() {
            return candidate.to_path_buf();
        }
    }
    start.to_path_buf()
}

fn resolve_project_root(start: PathBuf, discover_git: bool) -> PathBuf {
    let canonical = start.canonicalize().unwrap_or(start);
    if discover_git {
        discover_project_root(&canonical)
    } else {
        canonical
    }
}

pub(crate) fn hash_hex(input: &[u8]) -> String {
    hex::encode(Sha256::digest(input))
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryConfig, default_model_cache, discover_project_root, resolve_model_cache,
        resolve_project_root, revision_cache_component,
    };
    use std::fs;

    #[test]
    fn project_id_is_stable_and_collection_is_scoped() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("create project");
        let first = MemoryConfig::new(
            project.clone(),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let second = MemoryConfig::new(
            project,
            temp.path().join("other-data"),
            temp.path().join("other-cache"),
        );

        assert_eq!(first.project_id(), second.project_id());
        assert!(first.collection_dir().starts_with(temp.path().join("data")));
    }

    #[test]
    fn discovers_nearest_git_root() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let root = temp.path().join("repo");
        let nested = root.join("src/deep");
        fs::create_dir_all(root.join(".git")).expect("create git marker");
        fs::create_dir_all(&nested).expect("create nested path");

        assert_eq!(discover_project_root(&nested), root);
    }

    #[test]
    fn explicit_project_root_does_not_expand_to_parent_git_repository() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let root = temp.path().join("repo");
        let nested = root.join("tests/demo");
        fs::create_dir_all(root.join(".git")).expect("create git marker");
        fs::create_dir_all(&nested).expect("create nested project");
        let expected = nested.canonicalize().expect("canonicalize nested project");

        assert_eq!(resolve_project_root(nested, false), expected);
    }

    #[test]
    fn default_model_cache_is_versioned_by_model_revision() {
        assert_eq!(
            default_model_cache(std::path::Path::new("/data"), "abc123"),
            std::path::Path::new("/data/opencode/memory/models/abc123")
        );
    }

    #[test]
    fn explicit_model_cache_is_used_without_appending_revision() {
        assert_eq!(
            resolve_model_cache(
                Some(std::path::PathBuf::from("/custom/cache")),
                std::path::Path::new("/data"),
                "abc123",
            ),
            std::path::Path::new("/custom/cache")
        );
    }

    #[test]
    fn unsafe_model_revision_is_confined_to_one_cache_component() {
        let component = revision_cache_component("../../outside");

        assert!(component.starts_with("revision-"));
        assert!(!component.contains('/'));
        assert!(!component.contains(".."));
    }
}
