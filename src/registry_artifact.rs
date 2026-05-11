use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Debug)]
pub(crate) struct RegistryArtifactStore {
    path: PathBuf,
    update_lock: tokio::sync::Mutex<()>,
}

impl RegistryArtifactStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            update_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn lock_update(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.update_lock.lock().await
    }

    pub(crate) async fn load(&self) -> Result<Option<String>, RegistryArtifactStoreError> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(contents) => Ok(Some(contents)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(RegistryArtifactStoreError::Read(e)),
        }
    }

    pub(crate) async fn persist(&self, artifact: &str) -> Result<(), RegistryArtifactStoreError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(RegistryArtifactStoreError::CreateDir)?;
        }

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(RegistryArtifactStoreError::InvalidPath)?;
        let tmp_path = self
            .path
            .with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));

        if let Err(e) = write_private_artifact(&tmp_path, artifact).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }

        if let Err(e) = set_restrictive_permissions(&tmp_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }

        if let Err(e) = tokio::fs::rename(&tmp_path, &self.path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(RegistryArtifactStoreError::Rename(e));
        }

        Ok(())
    }

    pub(crate) async fn restore(
        &self,
        previous: Option<&str>,
    ) -> Result<(), RegistryArtifactStoreError> {
        match previous {
            Some(artifact) => self.persist(artifact).await,
            None => match tokio::fs::remove_file(&self.path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(RegistryArtifactStoreError::Remove(e)),
            },
        }
    }
}

pub(crate) fn artifact_sha256(artifact: &str) -> String {
    format!("{:x}", Sha256::digest(artifact.as_bytes()))
}

async fn write_private_artifact(
    path: &Path,
    artifact: &str,
) -> Result<(), RegistryArtifactStoreError> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(path)
        .await
        .map_err(RegistryArtifactStoreError::Write)?;
    file.write_all(artifact.as_bytes())
        .await
        .map_err(RegistryArtifactStoreError::Write)?;
    file.sync_all()
        .await
        .map_err(RegistryArtifactStoreError::Write)
}

async fn set_restrictive_permissions(path: &Path) -> Result<(), RegistryArtifactStoreError> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))
        .await
        .map_err(RegistryArtifactStoreError::Permissions)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryArtifactStoreError {
    #[error("invalid private registry artifact path")]
    InvalidPath,
    #[error("failed to read private registry artifact")]
    Read(#[source] std::io::Error),
    #[error("failed to create private registry artifact directory")]
    CreateDir(#[source] std::io::Error),
    #[error("failed to write private registry artifact")]
    Write(#[source] std::io::Error),
    #[error("failed to set private registry artifact permissions")]
    Permissions(#[source] std::io::Error),
    #[error("failed to activate private registry artifact")]
    Rename(#[source] std::io::Error),
    #[error("failed to remove private registry artifact")]
    Remove(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn load_returns_none_when_artifact_is_missing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("private-registry.data"));

        let artifact = store.load().await.expect("load artifact");

        assert_eq!(artifact, None);
    }

    #[tokio::test]
    async fn persist_creates_parent_directory_and_load_reads_artifact() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir
            .path()
            .join("nested")
            .join("private")
            .join("registry.data");
        let store = RegistryArtifactStore::new(path.clone());

        store
            .persist("data:text/plain;base64,c2VjcmV0")
            .await
            .expect("persist artifact");

        assert_eq!(
            store.load().await.expect("load artifact"),
            Some("data:text/plain;base64,c2VjcmV0".to_string())
        );
        assert!(path.exists());
    }

    #[tokio::test]
    async fn persist_replaces_existing_artifact() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("registry.data"));

        store
            .persist("first")
            .await
            .expect("persist first artifact");
        store
            .persist("second")
            .await
            .expect("persist replacement artifact");

        assert_eq!(
            store.load().await.expect("load artifact"),
            Some("second".to_string())
        );
    }

    #[tokio::test]
    async fn persist_does_not_leave_temp_files_after_success() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("registry.data"));

        store.persist("artifact").await.expect("persist artifact");

        let entries = std::fs::read_dir(dir.path())
            .expect("read temp dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect dir entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name(), "registry.data");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persist_sets_restrictive_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("registry.data");
        let store = RegistryArtifactStore::new(path.clone());

        store.persist("artifact").await.expect("persist artifact");

        let mode = std::fs::metadata(path)
            .expect("read metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[tokio::test]
    async fn restore_rewrites_previous_artifact() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("registry.data"));

        store.persist("current").await.expect("persist current");
        store
            .restore(Some("previous"))
            .await
            .expect("restore previous");

        assert_eq!(
            store.load().await.expect("load artifact"),
            Some("previous".to_string())
        );
    }

    #[tokio::test]
    async fn restore_none_removes_artifact_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("registry.data"));

        store.persist("current").await.expect("persist current");
        store.restore(None).await.expect("remove artifact");
        store.restore(None).await.expect("remove missing artifact");

        assert_eq!(store.load().await.expect("load artifact"), None);
    }

    #[tokio::test]
    async fn lock_update_serializes_access() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = RegistryArtifactStore::new(dir.path().join("registry.data"));
        let guard = store.lock_update().await;

        let blocked = tokio::time::timeout(Duration::from_millis(25), store.lock_update()).await;
        assert!(blocked.is_err());

        drop(guard);

        let unblocked = tokio::time::timeout(Duration::from_secs(1), store.lock_update()).await;
        assert!(unblocked.is_ok());
    }

    #[test]
    fn artifact_sha256_matches_known_digest() {
        assert_eq!(
            artifact_sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
