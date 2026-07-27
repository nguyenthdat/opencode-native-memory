use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use rustix::fs::{Mode, OFlags, open};
use tokio::net::UnixListener;

const MAX_SOCKET_PATH_BYTES: usize = 100;

pub(crate) struct EndpointGuard {
    endpoint: PathBuf,
    endpoint_identity: (u64, u64),
    lifetime_lock: File,
}

impl EndpointGuard {
    pub(crate) fn bind(endpoint: &Path) -> Result<(UnixListener, Self, u32)> {
        Self::bind_inner(endpoint, true)
    }

    fn bind_inner(endpoint: &Path, enforce_default: bool) -> Result<(UnixListener, Self, u32)> {
        anyhow::ensure!(
            endpoint.is_absolute(),
            "daemon endpoint must be an absolute path"
        );
        anyhow::ensure!(
            endpoint.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES,
            "daemon endpoint exceeds the supported Unix socket path limit: {}",
            endpoint.display()
        );
        let runtime_dir = endpoint
            .parent()
            .context("daemon endpoint has no runtime directory")?;
        let uid = rustix::process::getuid().as_raw();
        if enforce_default {
            anyhow::ensure!(
                endpoint == expected_endpoint(uid),
                "daemon endpoint must use the canonical per-user path {}",
                expected_endpoint(uid).display()
            );
        }
        ensure_runtime_directory(runtime_dir, uid)?;

        let lock_path = runtime_dir.join("daemon-lifetime.lock");
        let lifetime_lock = File::from(
            open(
                &lock_path,
                OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .with_context(|| format!("cannot open {}", lock_path.display()))?,
        );
        lifetime_lock.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let lock_metadata = lifetime_lock.metadata()?;
        anyhow::ensure!(
            lock_metadata.is_file(),
            "daemon lifetime lock is not a regular file"
        );
        anyhow::ensure!(
            lock_metadata.uid() == uid,
            "daemon lifetime lock has a foreign owner"
        );
        anyhow::ensure!(
            lock_metadata.mode() & 0o777 == 0o600,
            "daemon lifetime lock must use mode 0600"
        );
        anyhow::ensure!(
            lock_metadata.nlink() == 1,
            "daemon lifetime lock has multiple links"
        );
        lifetime_lock.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!(
                "another native memory daemon owns {}: {error}",
                lock_path.display()
            )
        })?;

        validate_or_remove_stale_endpoint(endpoint, uid)?;
        let listener = UnixListener::bind(endpoint)
            .with_context(|| format!("cannot bind daemon endpoint {}", endpoint.display()))?;
        std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot restrict daemon endpoint {}", endpoint.display()))?;
        let endpoint_metadata = std::fs::symlink_metadata(endpoint)?;
        anyhow::ensure!(
            endpoint_metadata.file_type().is_socket(),
            "bound daemon endpoint changed type"
        );
        anyhow::ensure!(
            endpoint_metadata.uid() == uid,
            "bound daemon endpoint changed owner"
        );
        anyhow::ensure!(
            endpoint_metadata.mode() & 0o777 == 0o600,
            "daemon endpoint must use mode 0600"
        );

        Ok((
            listener,
            Self {
                endpoint: endpoint.to_path_buf(),
                endpoint_identity: (endpoint_metadata.dev(), endpoint_metadata.ino()),
                lifetime_lock,
            },
            uid,
        ))
    }
}

pub(crate) fn expected_endpoint(uid: u32) -> PathBuf {
    #[cfg(target_os = "linux")]
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("opencode-memory"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencode-memory"));
    #[cfg(not(target_os = "linux"))]
    let runtime_dir = std::env::temp_dir().join("opencode-memory");
    let endpoint = runtime_dir.join("daemon.sock");
    if endpoint.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES {
        endpoint
    } else {
        PathBuf::from(format!("/tmp/opencode-memory-{uid}/daemon.sock"))
    }
}

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.endpoint)
            && metadata.file_type().is_socket()
            && (metadata.dev(), metadata.ino()) == self.endpoint_identity
        {
            let _ = std::fs::remove_file(&self.endpoint);
        }
        let _ = FileExt::unlock(&self.lifetime_lock);
    }
}

fn ensure_runtime_directory(path: &Path, uid: u32) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path).with_context(|| {
            format!("cannot create daemon runtime directory {}", path.display())
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect daemon runtime directory {}", path.display()))?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "daemon runtime directory is a symlink"
    );
    anyhow::ensure!(metadata.is_dir(), "daemon runtime path is not a directory");
    anyhow::ensure!(
        metadata.uid() == uid,
        "daemon runtime directory has a foreign owner"
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o700,
        "daemon runtime directory must use mode 0700"
    );
    Ok(())
}

fn validate_or_remove_stale_endpoint(endpoint: &Path, uid: u32) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(endpoint) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("cannot inspect daemon endpoint"),
    };
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "daemon endpoint is a symlink"
    );
    anyhow::ensure!(
        metadata.file_type().is_socket(),
        "daemon endpoint is not a Unix socket"
    );
    anyhow::ensure!(metadata.uid() == uid, "daemon endpoint has a foreign owner");
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o600,
        "daemon endpoint must use mode 0600"
    );
    match StdUnixStream::connect(endpoint) {
        Ok(_) => anyhow::bail!(
            "a live native memory daemon already owns {}",
            endpoint.display()
        ),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot determine daemon endpoint liveness for {}",
                    endpoint.display()
                )
            });
        }
    }
    std::fs::remove_file(endpoint)
        .with_context(|| format!("cannot remove stale daemon endpoint {}", endpoint.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::sync::Mutex;

    use super::*;

    static ENDPOINT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rejects_symlink_endpoint() {
        let _guard = ENDPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = tempfile::tempdir().expect("create temp dir");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restrict temp dir");
        let target = temp.path().join("target");
        File::create(&target).expect("create target");
        let endpoint = temp.path().join("daemon.sock");
        symlink(&target, &endpoint).expect("create symlink");

        let error = match EndpointGuard::bind_inner(&endpoint, false) {
            Ok(_) => panic!("accepted symlink endpoint"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symlink"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifetime_lock_allows_only_one_daemon_per_runtime_directory() {
        let _guard = ENDPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let runtime = tempfile::tempdir().expect("create temp dir");
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restrict temp dir");
        let (_listener, _guard, _) =
            EndpointGuard::bind_inner(&runtime.path().join("daemon.sock"), false)
                .expect("bind first daemon");

        let error = match EndpointGuard::bind_inner(&runtime.path().join("other.sock"), false) {
            Ok(_) => panic!("bound a second user daemon"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("another native memory daemon"));
    }
}
