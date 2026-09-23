//! Development-only secret store: one file per secret, mode 0600.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::SecretStore;

pub struct FileSecrets {
    dir: PathBuf,
}

impl FileSecrets {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating secrets dir {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("invalid secret name {name:?}");
        }
        Ok(self.dir.join(name))
    }
}

#[async_trait]
impl SecretStore for FileSecrets {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        let path = self.path(name)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        let path = self.path(name)?;
        write_private(&path, value).await
    }
}

async fn write_private(path: &Path, value: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, value).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
    }
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecrets::new(dir.path()).unwrap();
        assert_eq!(store.get("nope").await.unwrap(), None);
        store.set("jwt-signing-key", "abc").await.unwrap();
        assert_eq!(
            store.get("jwt-signing-key").await.unwrap().as_deref(),
            Some("abc")
        );
        store.set("jwt-signing-key", "def").await.unwrap();
        assert_eq!(
            store.get("jwt-signing-key").await.unwrap().as_deref(),
            Some("def")
        );
    }

    #[tokio::test]
    async fn rejects_path_like_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecrets::new(dir.path()).unwrap();
        assert!(store.get("../etc/passwd").await.is_err());
        assert!(store.set("a/b", "x").await.is_err());
    }
}
