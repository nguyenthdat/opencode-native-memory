use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

const DATA_SUBDIR: &str = "opencode/native-memory";
const CACHE_SUBDIR: &str = "opencode/native-memory/models";

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    project_root: PathBuf,
    project_id: String,
    data_root: PathBuf,
    model_cache: PathBuf,
}

impl MemoryConfig {
    /// Discover project, storage, and model-cache paths from the environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the current working directory cannot be determined.
    pub fn discover() -> Result<Self> {
        let current = match env::var_os("OPENCODE_MEMORY_PROJECT_ROOT") {
            Some(value) => PathBuf::from(value),
            None => env::current_dir().context("cannot determine the current project directory")?,
        };
        let canonical = current.canonicalize().unwrap_or(current);
        let project_root = discover_project_root(&canonical);

        let data_root = env_path("OPENCODE_MEMORY_DATA_DIR")
            .unwrap_or_else(|| default_data_home().join(DATA_SUBDIR));
        let model_cache = env_path("OPENCODE_MEMORY_MODEL_CACHE")
            .unwrap_or_else(|| default_cache_home().join(CACHE_SUBDIR));

        Ok(Self::new(project_root, data_root, model_cache))
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
        }
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

fn default_data_home() -> PathBuf {
    if let Some(path) = env_path("XDG_DATA_HOME") {
        return path;
    }
    home_dir().join(".local/share")
}

fn default_cache_home() -> PathBuf {
    if let Some(path) = env_path("XDG_CACHE_HOME") {
        return path;
    }
    home_dir().join(".cache")
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

pub(crate) fn hash_hex(input: &[u8]) -> String {
    hex::encode(Sha256::digest(input))
}

#[cfg(test)]
mod tests {
    use super::{MemoryConfig, discover_project_root};
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
}
