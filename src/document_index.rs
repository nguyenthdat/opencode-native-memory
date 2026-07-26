//! Incremental document discovery and a derived per-project index manifest.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};

use anyhow::{Context, Result, ensure};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::MemoryConfig;
use crate::document::is_supported_document_path;
use crate::validation::validate_ids;

const DOCUMENT_INDEX_FORMAT_VERSION: u32 = 1;
pub(crate) const MAX_INDEXED_DOCUMENTS: usize = 1_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedDocument {
    pub(crate) content_hash: String,
    pub(crate) memory_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentIndexManifest {
    format_version: u32,
    pub(crate) files: BTreeMap<String, IndexedDocument>,
}

impl Default for DocumentIndexManifest {
    fn default() -> Self {
        Self {
            format_version: DOCUMENT_INDEX_FORMAT_VERSION,
            files: BTreeMap::new(),
        }
    }
}

impl DocumentIndexManifest {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)
            .with_context(|| format!("cannot read document index {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid document index {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
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
            .with_context(|| format!("cannot install document index at {}", path.display()))?;
        sync_parent(path)?;
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.files.clear();
    }

    pub(crate) fn is_current(
        &self,
        path: &str,
        content_hash: &str,
        mut has_memory: impl FnMut(&str) -> bool,
    ) -> bool {
        self.files.get(path).is_some_and(|entry| {
            entry.content_hash == content_hash && entry.memory_ids.iter().all(|id| has_memory(id))
        })
    }

    pub(crate) fn missing_paths(&self, incoming: &HashSet<String>) -> Vec<String> {
        self.files
            .keys()
            .filter(|path| !incoming.contains(*path))
            .cloned()
            .collect()
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.format_version == DOCUMENT_INDEX_FORMAT_VERSION,
            "unsupported document index format {}; expected {DOCUMENT_INDEX_FORMAT_VERSION}",
            self.format_version
        );
        ensure!(
            self.files.len() <= MAX_INDEXED_DOCUMENTS,
            "document index exceeds {MAX_INDEXED_DOCUMENTS} files"
        );
        for (path, entry) in &self.files {
            validate_relative_path(path)?;
            ensure!(
                entry.content_hash.len() == 64
                    && entry
                        .content_hash
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()),
                "document index hash is invalid for {path}"
            );
            ensure!(
                !entry.memory_ids.is_empty(),
                "document index has no memory IDs for {path}"
            );
            validate_ids(&entry.memory_ids)?;
        }
        Ok(())
    }
}

pub(crate) struct DocumentDiscovery {
    pub(crate) paths: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) complete: bool,
}

pub(crate) fn discover_documents(config: &MemoryConfig) -> Result<DocumentDiscovery> {
    let root = config
        .project_root()
        .canonicalize()
        .context("cannot resolve project root for document discovery")?;
    let mut builder = WalkBuilder::new(&root);
    builder
        .follow_links(false)
        .hidden(true)
        .require_git(false)
        .sort_by_file_path(std::path::Path::cmp);

    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    let mut complete = true;
    for item in builder.build() {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                complete = false;
                warnings.push(format!("document discovery: {error}"));
                continue;
            }
        };
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            || !is_supported_document_path(entry.path())
        {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .context("discovered document resolves outside the project root")?;
        if is_shared_memory_path(relative) {
            continue;
        }
        let Some(path) = relative.to_str() else {
            warnings.push(format!(
                "skipped non-UTF-8 document path: {}",
                relative.display()
            ));
            continue;
        };
        paths.push(path.replace('\\', "/"));
    }
    paths.sort();
    paths.dedup();
    ensure!(
        paths.len() <= MAX_INDEXED_DOCUMENTS,
        "document discovery found {} files; maximum is {MAX_INDEXED_DOCUMENTS}",
        paths.len()
    );
    Ok(DocumentDiscovery {
        paths,
        warnings,
        complete,
    })
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    ensure!(!value.is_empty(), "document index path cannot be empty");
    ensure!(!path.is_absolute(), "document index path must be relative");
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "document index path contains unsafe components: {value}"
    );
    Ok(())
}

fn is_shared_memory_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(value)) if value == ".opencode")
        && matches!(components.next(), Some(Component::Normal(value)) if value == "memory")
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)
            .with_context(|| format!("cannot open {} for sync", parent.display()))?
            .sync_all()
            .with_context(|| format!("cannot sync {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .context("cannot restrict document index permissions")
}

#[cfg(not(unix))]
fn set_private_file_permissions(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{DocumentIndexManifest, IndexedDocument, discover_documents};
    use crate::MemoryConfig;

    #[test]
    fn discovery_respects_ignore_files_and_supported_extensions() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        std::fs::create_dir(directory.path().join("docs"))
            .expect("docs directory should be created");
        std::fs::write(directory.path().join("README.md"), "root")
            .expect("root document should be written");
        std::fs::write(directory.path().join("docs/guide.html"), "guide")
            .expect("guide should be written");
        std::fs::write(directory.path().join("docs/ignored.pdf"), "ignored")
            .expect("ignored document should be written");
        std::fs::write(directory.path().join("docs/code.rs"), "fn main() {}")
            .expect("code should be written");
        std::fs::write(directory.path().join(".gitignore"), "docs/ignored.pdf\n")
            .expect("ignore file should be written");
        let config = MemoryConfig::new(
            directory.path().to_path_buf(),
            directory.path().join("data"),
            directory.path().join("models"),
        );

        let discovery = discover_documents(&config).expect("documents should be discovered");

        assert!(discovery.complete);
        assert_eq!(discovery.paths, ["README.md", "docs/guide.html"]);
    }

    #[test]
    fn manifest_round_trip_preserves_index_ownership() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("document-index.json");
        let mut manifest = DocumentIndexManifest::default();
        manifest.files.insert(
            "docs/guide.md".to_string(),
            IndexedDocument {
                content_hash: "a".repeat(64),
                memory_ids: vec!["mem_00000000000000000000000000000000".to_string()],
            },
        );

        manifest.save(&path).expect("manifest should save");
        let loaded = DocumentIndexManifest::load(&path).expect("manifest should load");

        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files["docs/guide.md"].content_hash, "a".repeat(64));
    }

    #[test]
    fn manifest_detects_changed_missing_and_partially_deleted_documents() {
        let mut manifest = DocumentIndexManifest::default();
        manifest.files.insert(
            "docs/guide.md".to_string(),
            IndexedDocument {
                content_hash: "a".repeat(64),
                memory_ids: vec!["mem_00000000000000000000000000000000".to_string()],
            },
        );
        manifest.files.insert(
            "docs/removed.pdf".to_string(),
            IndexedDocument {
                content_hash: "b".repeat(64),
                memory_ids: vec!["mem_11111111111111111111111111111111".to_string()],
            },
        );

        assert!(manifest.is_current("docs/guide.md", &"a".repeat(64), |_| true));
        assert!(!manifest.is_current("docs/guide.md", &"c".repeat(64), |_| true));
        assert!(!manifest.is_current("docs/guide.md", &"a".repeat(64), |_| false));
        assert_eq!(
            manifest.missing_paths(&HashSet::from(["docs/guide.md".to_string()])),
            ["docs/removed.pdf"]
        );
    }
}
