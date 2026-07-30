//! Concrete zvec collection storage and its v1 manifest/schema.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use zvec_rust::{Collection, CollectionSchema, DataType, FieldSchema, IndexParams, MetricType};

use crate::MemoryConfig;

const COLLECTION_SCHEMA_VERSION: u32 = 1;
pub(crate) const RESULT_FIELDS: [&str; 9] = [
    "title",
    "content",
    "kind",
    "importance",
    "tags",
    "source",
    "content_hash",
    "created_at",
    "updated_at",
];

static ZVEC_INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    schema_version: u32,
    project_root: String,
    project_id: String,
    embedding_model: String,
    embedding_dimension: usize,
    #[serde(default)]
    configuration_fingerprint: Option<String>,
    zvec_version: String,
    created_at_ms: i64,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    generation_id: Option<String>,
}

pub(crate) fn initialize() -> Result<()> {
    ZVEC_INITIALIZED
        .get_or_init(|| zvec_rust::initialize(None).map_err(|error| error.to_string()))
        .clone()
        .map_err(|error| anyhow!("cannot initialize zvec: {error}"))
}

pub(crate) fn open_collection(
    config: &MemoryConfig,
    embedding_model: &str,
    embedding_dimension: usize,
    now_ms: i64,
) -> Result<Collection> {
    open_collection_at(
        config,
        &config.collection_dir(),
        &config.project_data_dir().join("manifest.json"),
        embedding_model,
        embedding_dimension,
        &config.configuration_fingerprint()?,
        None,
        None,
        now_ms,
    )
}

pub(crate) fn open_existing_collection(
    config: &MemoryConfig,
    embedding_model: &str,
    embedding_dimension: usize,
    now_ms: i64,
) -> Result<Collection> {
    ensure!(
        config.collection_dir().is_dir()
            && config.project_data_dir().join("manifest.json").is_file(),
        "legacy embedding generation collection is missing"
    );
    open_collection(config, embedding_model, embedding_dimension, now_ms)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open_generation_collection(
    config: &MemoryConfig,
    generation_id: &str,
    profile_id: &str,
    profile_fingerprint: &str,
    embedding_model: &str,
    embedding_dimension: usize,
    now_ms: i64,
) -> Result<Collection> {
    let generation_dir = config.embedding_generation_dir(generation_id);
    secure_create_dir(&generation_dir)?;
    open_collection_at(
        config,
        &generation_dir.join("zvec"),
        &generation_dir.join("manifest.json"),
        embedding_model,
        embedding_dimension,
        profile_fingerprint,
        Some(profile_id),
        Some(generation_id),
        now_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_collection_at(
    config: &MemoryConfig,
    collection_path: &Path,
    manifest_path: &Path,
    embedding_model: &str,
    embedding_dimension: usize,
    expected_fingerprint: &str,
    profile_id: Option<&str>,
    generation_id: Option<&str>,
    now_ms: i64,
) -> Result<Collection> {
    let collection_path_text = path_text(collection_path)?;

    if manifest_path.exists() {
        ensure!(
            collection_path.exists(),
            "memory manifest exists but the zvec collection is missing: {}",
            collection_path.display()
        );
        let manifest: Manifest = serde_json::from_str(
            &fs::read_to_string(manifest_path)
                .with_context(|| format!("cannot read {}", manifest_path.display()))?,
        )
        .with_context(|| format!("invalid memory manifest: {}", manifest_path.display()))?;
        validate_manifest(
            config,
            &manifest,
            embedding_model,
            embedding_dimension,
            expected_fingerprint,
            profile_id,
            generation_id,
        )?;
        return Collection::open(&collection_path_text, None).map_err(Into::into);
    }

    ensure!(
        !collection_path.exists(),
        "zvec collection exists without a manifest: {}; move it aside or restore its manifest",
        collection_path.display()
    );
    let schema = collection_schema(embedding_dimension)?;
    let collection = Collection::create_and_open(&collection_path_text, &schema, None)?;
    let manifest = Manifest {
        schema_version: COLLECTION_SCHEMA_VERSION,
        project_root: config.project_root().display().to_string(),
        project_id: config.project_id().to_string(),
        embedding_model: embedding_model.to_string(),
        embedding_dimension,
        configuration_fingerprint: Some(expected_fingerprint.to_string()),
        zvec_version: zvec_rust::version().clone(),
        created_at_ms: now_ms,
        profile_id: profile_id.map(str::to_string),
        generation_id: generation_id.map(str::to_string),
    };
    write_manifest(manifest_path, &manifest)?;
    Ok(collection)
}

pub(crate) fn acquire_writer_lock(project_dir: &Path) -> Result<File> {
    let lock_path = project_dir.join("writer.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open memory writer lock: {}", lock_path.display()))?;
    set_private_file_permissions(&file)?;
    file.try_lock_exclusive().map_err(|error| {
        anyhow!(
            "project store is already owned by another native memory engine ({}): {error}",
            lock_path.display()
        )
    })?;
    Ok(file)
}

pub(crate) fn secure_create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn ensure_write_succeeded(
    operation: &str,
    result: &zvec_rust::WriteResult,
) -> Result<()> {
    if result.error_count == 0 {
        return Ok(());
    }
    let details = result
        .results
        .iter()
        .filter(|item| !item.is_success())
        .map(|item| item.message.as_str())
        .filter(|message| !message.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "cannot {operation}: {} document(s) failed{}{}",
        result.error_count,
        if details.is_empty() { "" } else { ": " },
        details
    )
}

fn collection_schema(embedding_dimension: usize) -> Result<CollectionSchema> {
    Ok(CollectionSchema::builder("opencode_project_memory")
        .add_field(FieldSchema::new("title", DataType::String, false, 0)?)
        .add_field(FieldSchema::new("content", DataType::String, false, 0)?)
        .add_indexed_field(
            "search_text",
            DataType::String,
            IndexParams::fts(None, None, None)?,
        )
        .add_indexed_field("kind", DataType::String, IndexParams::invert(false, false)?)
        .add_field(FieldSchema::new("importance", DataType::Float, false, 0)?)
        .add_field(FieldSchema::new("tags", DataType::String, false, 0)?)
        .add_field(FieldSchema::new("source", DataType::String, false, 0)?)
        .add_indexed_field(
            "content_hash",
            DataType::String,
            IndexParams::invert(false, false)?,
        )
        .add_indexed_field(
            "created_at",
            DataType::Int64,
            IndexParams::invert(true, false)?,
        )
        .add_field(FieldSchema::new("updated_at", DataType::Int64, false, 0)?)
        .add_vector_field(
            "embedding",
            DataType::VectorFp32,
            u32::try_from(embedding_dimension)?,
            IndexParams::hnsw(MetricType::Cosine, 16, 200)?,
        )
        .max_doc_count_per_segment(10_000)
        .build()?)
}

fn validate_manifest(
    config: &MemoryConfig,
    manifest: &Manifest,
    embedding_model: &str,
    embedding_dimension: usize,
    expected_fingerprint: &str,
    profile_id: Option<&str>,
    generation_id: Option<&str>,
) -> Result<()> {
    ensure!(
        manifest.schema_version == COLLECTION_SCHEMA_VERSION,
        "unsupported memory schema version {}; expected {COLLECTION_SCHEMA_VERSION}",
        manifest.schema_version
    );
    ensure!(
        manifest.project_id == config.project_id(),
        "memory collection belongs to a different project"
    );
    ensure!(
        manifest.embedding_model == embedding_model
            && manifest.embedding_dimension == embedding_dimension,
        "memory embedding model mismatch: collection uses {} ({} dimensions), configured model is {} ({} dimensions); changing models requires re-indexing the project collection",
        manifest.embedding_model,
        manifest.embedding_dimension,
        embedding_model,
        embedding_dimension
    );
    if let Some(expected) = &manifest.configuration_fingerprint {
        ensure!(
            expected == expected_fingerprint,
            "memory configuration fingerprint mismatch; vector-affecting embedding settings changed and require re-indexing the project collection"
        );
    }
    ensure!(
        manifest.profile_id.as_deref() == profile_id
            && manifest.generation_id.as_deref() == generation_id,
        "memory collection generation identity mismatch"
    );
    Ok(())
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("cannot create {}", temporary.display()))?;
    set_private_file_permissions(&file)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("cannot install memory manifest at {}", path.display()))?;
    Ok(())
}

fn set_private_file_permissions(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("memory path is not valid UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{COLLECTION_SCHEMA_VERSION, Manifest, acquire_writer_lock, validate_manifest};
    use crate::MemoryConfig;

    #[test]
    fn writer_lock_rejects_a_second_owner_and_recovers_after_drop() {
        let directory = tempfile::tempdir().expect("create temporary project directory");
        let first = acquire_writer_lock(directory.path()).expect("acquire first writer lock");

        let error = acquire_writer_lock(directory.path()).expect_err("reject second writer lock");
        assert!(error.to_string().contains("another native memory engine"));

        drop(first);
        acquire_writer_lock(directory.path()).expect("reacquire released writer lock");
    }

    #[test]
    fn manifest_rejects_a_vector_affecting_configuration_change() {
        let directory = tempfile::tempdir().expect("create temporary project directory");
        let config = MemoryConfig::new(
            directory.path().to_path_buf(),
            directory.path().join("data"),
            directory.path().join("cache"),
        );
        let manifest = Manifest {
            schema_version: COLLECTION_SCHEMA_VERSION,
            project_root: config.project_root().display().to_string(),
            project_id: config.project_id().to_string(),
            embedding_model: "model-a".to_string(),
            embedding_dimension: 4,
            configuration_fingerprint: Some("different-fingerprint".to_string()),
            zvec_version: "test".to_string(),
            created_at_ms: 0,
            profile_id: None,
            generation_id: None,
        };

        let error = validate_manifest(
            &config,
            &manifest,
            "model-a",
            4,
            &config.configuration_fingerprint().expect("fingerprint"),
            None,
            None,
        )
        .expect_err("reject fingerprint mismatch");
        assert!(
            error
                .to_string()
                .contains("configuration fingerprint mismatch")
        );
    }
}
