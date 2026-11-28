use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use tempfile::NamedTempFile;

use super::protocol::{validate_relative_key, validate_relative_prefix};
use super::{RemoteProbeInfo, SyncError, SyncRemote};

/// `SyncRemote` adapter backed by a local filesystem directory (Syncthing, iCloud, Dropbox, OneDrive).
#[derive(Debug, Clone)]
pub struct LocalDirRemote {
    root: PathBuf,
}

impl LocalDirRemote {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve_key_path(&self, key: &str) -> Result<PathBuf, SyncError> {
        if self.root.as_os_str().is_empty() {
            return Err(SyncError::Config(
                "local-dir remote root path must not be empty".to_string(),
            ));
        }
        let valid = validate_relative_key(key)?;
        let mut path = self.root.clone();
        for seg in valid.split('/') {
            let rel = Path::new(seg);
            if rel.components().count() != 1
                || !matches!(rel.components().next(), Some(Component::Normal(_)))
            {
                return Err(SyncError::InvalidKey(format!(
                    "unsafe path segment in key: {key}"
                )));
            }
            path.push(seg);
        }
        Ok(path)
    }
}

fn collect_relative_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), SyncError> {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(SyncError::Io(err)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name_lossy = file_name.to_string_lossy();
        if name_lossy.starts_with(".tmp") {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_relative_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).map_err(|e| {
                SyncError::Remote(format!("failed to compute relative path under root: {e}"))
            })?;
            let normalized = rel
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            if !normalized.is_empty() {
                out.push(normalized);
            }
        }
    }

    Ok(())
}

impl SyncRemote for LocalDirRemote {
    fn probe(&self) -> Result<RemoteProbeInfo, SyncError> {
        if self.root.as_os_str().is_empty() {
            return Err(SyncError::Config(
                "local-dir remote root path must not be empty".to_string(),
            ));
        }
        fs::create_dir_all(&self.root)?;
        let meta = fs::metadata(&self.root)?;
        if !meta.is_dir() {
            return Err(SyncError::Config(format!(
                "local-dir remote path is not a directory: {}",
                self.root.display()
            )));
        }
        // Verify write/delete capability with an automatically removed temp file.
        let _probe_file = NamedTempFile::new_in(&self.root)?;
        Ok(RemoteProbeInfo {
            backend: "local-dir".to_string(),
            detail: self.root.display().to_string(),
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        let norm_prefix = validate_relative_prefix(prefix)?;
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        let mut all_keys = Vec::new();
        collect_relative_files(&self.root, &self.root, &mut all_keys)?;

        if !norm_prefix.is_empty() {
            let dir_prefix = format!("{norm_prefix}/");
            all_keys.retain(|k| k == &norm_prefix || k.starts_with(&dir_prefix));
        }
        all_keys.sort();
        Ok(all_keys)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError> {
        let path = self.resolve_key_path(key)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(SyncError::Io(err)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], _content_type: &str) -> Result<(), SyncError> {
        let dest = self.resolve_key_path(key)?;
        let parent = dest
            .parent()
            .ok_or_else(|| SyncError::InvalidKey(format!("key has no parent directory: {key}")))?;
        fs::create_dir_all(parent)?;

        let mut tmp = NamedTempFile::new_in(parent)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&dest).map_err(|e| SyncError::Io(e.error))?;
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SyncError> {
        let path = self.resolve_key_path(key)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(SyncError::Io(err)),
        }

        let mut cur = path.parent();
        while let Some(dir) = cur {
            if dir == self.root {
                break;
            }
            match fs::remove_dir(dir) {
                Ok(()) => cur = dir.parent(),
                Err(_) => break,
            }
        }
        Ok(())
    }
}
