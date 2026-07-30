use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

pub(crate) fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: usize,
) -> Result<()> {
    let encoded = serde_json::to_vec_pretty(value)?;
    ensure!(
        encoded.len() <= max_bytes,
        "JSON state exceeds {max_bytes} bytes"
    );
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
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("cannot install JSON state at {}", path.display()))?;
    sync_parent(path)
}

pub(crate) fn remove_file_durable(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
        sync_parent(path)?;
    }
    Ok(())
}

pub(crate) fn remove_dir_all_durable(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("cannot remove {}", path.display()))?;
        sync_parent(path)?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
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
