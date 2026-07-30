use std::env;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DATA_SUBDIR: &str = "opencode/memory";
const MODEL_CACHE_SUBDIR: &str = "opencode/memory/models";
const DEFAULT_MODEL_REPO: &str = "Qwen/Qwen3-Embedding-4B-GGUF";
const DEFAULT_MODEL_FILE: &str = "Qwen3-Embedding-4B-Q4_K_M.gguf";
const DEFAULT_MODEL_REVISION: &str = "f4602530db1d980e16da9d7d3a70294cf5c190be";
const DEFAULT_QUERY_TEMPLATE: &str = "Instruct: Given a code search query, retrieve relevant passages that answer the query\nQuery:{text}";

/// Runtime configuration for a llama.cpp-compatible GGUF embedding model.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
    pub(crate) fn discover() -> Result<Self> {
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
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.query_template.contains("{text}") && self.passage_template.contains("{text}"),
            "embedding query and passage templates must contain {{text}}"
        );
        anyhow::ensure!(
            self.context_size > 0,
            "embedding context size must be greater than zero"
        );
        if let Some(dimension) = self.dimension {
            anyhow::ensure!(
                dimension > 0,
                "embedding dimension must be greater than zero"
            );
        }
        if let Some(threads) = self.threads {
            anyhow::ensure!(threads > 0, "embedding threads must be greater than zero");
        }
        Ok(())
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

    pub(crate) fn for_daemon(
        project_root: PathBuf,
        data_root: Option<PathBuf>,
        model_cache: Option<PathBuf>,
        mut embedding: EmbeddingConfig,
    ) -> Result<Self> {
        let project_root = canonicalize_existing_directory(&project_root)
            .context("cannot resolve daemon project root")?;
        let data_home = default_data_home();
        let data_root = canonicalize_nearest_existing(
            &data_root.unwrap_or_else(|| data_home.join(DATA_SUBDIR)),
        )?;
        let model_cache = canonicalize_nearest_existing(
            &model_cache
                .unwrap_or_else(|| resolve_model_cache(None, &data_home, &embedding.revision)),
        )?;
        if let Some(model_path) = &embedding.model_path {
            embedding.model_path = Some(canonicalize_nearest_existing(model_path)?);
        }
        Ok(Self::new(project_root, data_root, model_cache).with_embedding(embedding))
    }

    /// Override the embedding model configuration.
    #[must_use]
    pub fn with_embedding(mut self, embedding: EmbeddingConfig) -> Self {
        self.embedding = embedding;
        self
    }

    pub(crate) fn set_embedding(&mut self, embedding: EmbeddingConfig) {
        self.embedding = embedding;
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

    #[must_use]
    pub(crate) fn graph_state_path(&self) -> PathBuf {
        self.project_data_dir().join("knowledge-graph.json")
    }

    #[must_use]
    pub(crate) fn graph_pending_path(&self) -> PathBuf {
        self.project_data_dir().join("knowledge-graph.pending.json")
    }

    #[must_use]
    pub(crate) fn active_embedding_path(&self) -> PathBuf {
        self.project_data_dir().join("active-embedding.json")
    }

    #[must_use]
    pub(crate) fn model_switch_path(&self) -> PathBuf {
        self.project_data_dir().join("model-switch.json")
    }

    #[must_use]
    pub(crate) fn embedding_generations_dir(&self) -> PathBuf {
        self.project_data_dir().join("embedding-generations")
    }

    #[must_use]
    pub(crate) fn embedding_generation_dir(&self, generation_id: &str) -> PathBuf {
        self.embedding_generations_dir().join(generation_id)
    }

    pub(crate) fn actor_compatibility_fingerprint(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hash_fingerprint_field(&mut hasher, self.project_root.to_string_lossy().as_bytes());
        hash_fingerprint_field(&mut hasher, self.project_id.as_bytes());
        hash_fingerprint_field(&mut hasher, b"memory-schema-v4");
        hash_fingerprint_field(&mut hasher, b"single-writer-project-actor-v1");
        if !self.active_embedding_path().is_file() {
            hash_fingerprint_field(
                &mut hasher,
                self.embedding_profile_fingerprint()?.as_bytes(),
            );
        }
        Ok(hex::encode(hasher.finalize()))
    }

    #[must_use]
    pub(crate) fn document_index_path(&self) -> PathBuf {
        self.project_data_dir().join("document-index.json")
    }

    pub(crate) fn canonical_store_key(&self) -> Result<PathBuf> {
        let store = absolute_normalized(&self.project_data_dir())?;
        if let Ok(metadata) = std::fs::symlink_metadata(&store) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "project data directory must not be a symlink: {}",
                store.display()
            );
        }
        canonicalize_nearest_existing(&store)
    }

    pub(crate) fn configuration_fingerprint(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hash_fingerprint_field(&mut hasher, self.project_root.to_string_lossy().as_bytes());
        hash_fingerprint_field(&mut hasher, self.project_id.as_bytes());
        let model_identity = match &self.embedding.model_path {
            Some(path) => {
                let canonical = canonicalize_nearest_existing(path)?;
                format!(
                    "local:{}#sha256:{}",
                    canonical.display(),
                    file_sha256(&canonical)?
                )
            }
            None => format!(
                "hf:{}@{}/{}",
                self.embedding.repo, self.embedding.revision, self.embedding.filename
            ),
        };
        hash_fingerprint_field(&mut hasher, model_identity.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.pooling.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.attention.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.query_template.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.passage_template.as_bytes());
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.add_bos)]);
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.append_eos)]);
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.normalize)]);
        hash_fingerprint_field(
            &mut hasher,
            &self.embedding.dimension.unwrap_or_default().to_le_bytes(),
        );
        hash_fingerprint_field(&mut hasher, &self.embedding.context_size.to_le_bytes());
        hash_fingerprint_field(&mut hasher, b"memory-schema-v4");
        Ok(hex::encode(hasher.finalize()))
    }

    pub(crate) fn embedding_profile_fingerprint(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        let model_identity = match &self.embedding.model_path {
            Some(path) => {
                let canonical = canonicalize_nearest_existing(path)?;
                format!(
                    "local:{}#sha256:{}",
                    canonical.display(),
                    file_sha256(&canonical)?
                )
            }
            None => format!(
                "hf:{}@{}/{}",
                self.embedding.repo, self.embedding.revision, self.embedding.filename
            ),
        };
        hash_fingerprint_field(&mut hasher, model_identity.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.pooling.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.attention.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.query_template.as_bytes());
        hash_fingerprint_field(&mut hasher, self.embedding.passage_template.as_bytes());
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.add_bos)]);
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.append_eos)]);
        hash_fingerprint_field(&mut hasher, &[u8::from(self.embedding.normalize)]);
        hash_fingerprint_field(
            &mut hasher,
            &self.embedding.dimension.unwrap_or_default().to_le_bytes(),
        );
        hash_fingerprint_field(&mut hasher, &self.embedding.context_size.to_le_bytes());
        Ok(hex::encode(hasher.finalize()))
    }
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| {
        format!(
            "cannot open embedding model for hashing: {}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot hash embedding model: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn hash_fingerprint_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn canonicalize_existing_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", path.display()))?;
    anyhow::ensure!(
        canonical.is_dir(),
        "path is not a directory: {}",
        canonical.display()
    );
    Ok(canonical)
}

fn canonicalize_nearest_existing(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_normalized(path)?;
    if absolute.exists() {
        return absolute
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", absolute.display()));
    }
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| {
            anyhow::anyhow!("path has no existing ancestor: {}", absolute.display())
        })?;
    }
    let suffix = absolute
        .strip_prefix(ancestor)
        .context("cannot derive canonical path suffix")?;
    Ok(ancestor
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", ancestor.display()))?
        .join(suffix))
}

fn absolute_normalized(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .context("cannot determine current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
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
        EmbeddingConfig, MemoryConfig, default_model_cache, discover_project_root,
        resolve_model_cache, resolve_project_root, revision_cache_component,
    };
    use std::fs;
    use std::os::unix::fs::symlink;

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

    #[test]
    fn canonical_store_key_deduplicates_symlinked_data_root_aliases() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        let data = temp.path().join("data");
        let alias = temp.path().join("data-alias");
        fs::create_dir_all(&project).expect("create project");
        fs::create_dir_all(&data).expect("create data root");
        symlink(&data, &alias).expect("create data alias");
        let direct = MemoryConfig::new(project.clone(), data, temp.path().join("cache"));
        let aliased = MemoryConfig::new(project, alias, temp.path().join("cache"));

        assert_eq!(
            direct.canonical_store_key().expect("direct key"),
            aliased.canonical_store_key().expect("aliased key")
        );
    }

    #[test]
    fn canonical_store_key_rejects_a_final_symlink() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("create project");
        let config =
            MemoryConfig::new(project, temp.path().join("data"), temp.path().join("cache"));
        fs::create_dir_all(
            config
                .project_data_dir()
                .parent()
                .expect("projects directory"),
        )
        .expect("create projects directory");
        let target = temp.path().join("other-store");
        fs::create_dir_all(&target).expect("create target");
        symlink(&target, config.project_data_dir()).expect("create final symlink");

        let error = config
            .canonical_store_key()
            .expect_err("reject final store symlink");
        assert!(error.to_string().contains("must not be a symlink"));
    }

    #[test]
    fn configuration_fingerprint_tracks_semantics_but_not_worker_threads() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("create project");
        let base = MemoryConfig::new(
            project.clone(),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let changed_threads = EmbeddingConfig {
            threads: Some(2),
            ..EmbeddingConfig::default()
        };
        let changed_threads = MemoryConfig::new(
            project.clone(),
            temp.path().join("data"),
            temp.path().join("cache"),
        )
        .with_embedding(changed_threads);
        let changed_template = EmbeddingConfig {
            query_template: "Different: {text}".to_string(),
            ..EmbeddingConfig::default()
        };
        let changed_template =
            MemoryConfig::new(project, temp.path().join("data"), temp.path().join("cache"))
                .with_embedding(changed_template);

        assert_eq!(
            base.configuration_fingerprint().expect("base fingerprint"),
            changed_threads
                .configuration_fingerprint()
                .expect("thread fingerprint")
        );
        assert_ne!(
            base.configuration_fingerprint().expect("base fingerprint"),
            changed_template
                .configuration_fingerprint()
                .expect("template fingerprint")
        );
    }

    #[test]
    fn actor_compatibility_fingerprint_separates_bootstrap_profiles() {
        let temp = tempfile::tempdir().expect("temp");
        let base = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let different = base.clone().with_embedding(EmbeddingConfig {
            repo: "Qwen/Qwen3-Embedding-0.6B-GGUF".to_string(),
            filename: "Qwen3-Embedding-0.6B-Q8_0.gguf".to_string(),
            revision: "370f27d7550e0def9b39c1f16d3fbaa13aa67728".to_string(),
            dimension: Some(1024),
            ..EmbeddingConfig::default()
        });
        assert_ne!(
            base.actor_compatibility_fingerprint().expect("fingerprint"),
            different
                .actor_compatibility_fingerprint()
                .expect("fingerprint")
        );
        assert_ne!(
            base.configuration_fingerprint().expect("fingerprint"),
            different.configuration_fingerprint().expect("fingerprint")
        );
    }

    #[test]
    fn configuration_fingerprint_tracks_local_model_content() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        let model = temp.path().join("model.gguf");
        fs::create_dir_all(&project).expect("create project");
        fs::write(&model, b"first model artifact").expect("write model");
        let embedding = EmbeddingConfig {
            model_path: Some(model.clone()),
            ..EmbeddingConfig::default()
        };
        let first = MemoryConfig::new(
            project.clone(),
            temp.path().join("data"),
            temp.path().join("cache"),
        )
        .with_embedding(embedding.clone())
        .configuration_fingerprint()
        .expect("first fingerprint");

        fs::write(&model, b"replacement artifact").expect("replace model");
        let second =
            MemoryConfig::new(project, temp.path().join("data"), temp.path().join("cache"))
                .with_embedding(embedding)
                .configuration_fingerprint()
                .expect("second fingerprint");

        assert_ne!(first, second);
    }
}
