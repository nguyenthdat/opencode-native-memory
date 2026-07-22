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
pub(crate) const EMBEDDING_DIMENSION: usize = 384;
pub(crate) const EMBEDDING_MODEL_NAME: &str = "intfloat/multilingual-e5-small";
pub(crate) const RESULT_FIELDS: [&str; 8] = [
    "title",
    "content",
    "kind",
    "importance",
    "tags",
    "source",
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
    zvec_version: String,
    created_at_ms: i64,
}

pub(crate) fn initialize() -> Result<()> {
    ZVEC_INITIALIZED
        .get_or_init(|| zvec_rust::initialize(None).map_err(|error| error.to_string()))
        .clone()
        .map_err(|error| anyhow!("cannot initialize zvec: {error}"))
}

pub(crate) fn open_collection(config: &MemoryConfig, now_ms: i64) -> Result<Collection> {
    let collection_path = config.collection_dir();
    let manifest_path = config.project_data_dir().join("manifest.json");
    let collection_path_text = path_text(&collection_path)?;

    if manifest_path.exists() {
        ensure!(
            collection_path.exists(),
            "memory manifest exists but the zvec collection is missing: {}",
            collection_path.display()
        );
        let manifest: Manifest = serde_json::from_str(
            &fs::read_to_string(&manifest_path)
                .with_context(|| format!("cannot read {}", manifest_path.display()))?,
        )
        .with_context(|| format!("invalid memory manifest: {}", manifest_path.display()))?;
        validate_manifest(config, &manifest)?;
        return Collection::open(&collection_path_text, None).map_err(Into::into);
    }

    ensure!(
        !collection_path.exists(),
        "zvec collection exists without a manifest: {}; move it aside or restore its manifest",
        collection_path.display()
    );
    let schema = collection_schema()?;
    let collection = Collection::create_and_open(&collection_path_text, &schema, None)?;
    let manifest = Manifest {
        schema_version: COLLECTION_SCHEMA_VERSION,
        project_root: config.project_root().display().to_string(),
        project_id: config.project_id().to_string(),
        embedding_model: EMBEDDING_MODEL_NAME.to_string(),
        embedding_dimension: EMBEDDING_DIMENSION,
        zvec_version: zvec_rust::version().clone(),
        created_at_ms: now_ms,
    };
    write_manifest(&manifest_path, &manifest)?;
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
            "another OpenCode process already owns this project's native memory writer lock ({}): {error}",
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

fn collection_schema() -> Result<CollectionSchema> {
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
            u32::try_from(EMBEDDING_DIMENSION)?,
            IndexParams::hnsw(MetricType::Cosine, 16, 200)?,
        )
        .max_doc_count_per_segment(10_000)
        .build()?)
}

fn validate_manifest(config: &MemoryConfig, manifest: &Manifest) -> Result<()> {
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
        manifest.embedding_model == EMBEDDING_MODEL_NAME
            && manifest.embedding_dimension == EMBEDDING_DIMENSION,
        "memory embedding model mismatch; migrate or remove the project collection"
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
