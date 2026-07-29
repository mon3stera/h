use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use tokio::{fs, sync::RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl FileFingerprint {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
            #[cfg(unix)]
            mtime_nsec: metadata.mtime_nsec(),
            #[cfg(unix)]
            ctime: metadata.ctime(),
            #[cfg(unix)]
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
pub(super) struct IndexedFile {
    pub(super) fingerprint: FileFingerprint,
    pub(super) line_starts: Vec<u64>,
    pub(super) scanned_to: u64,
    pub(super) total_lines: Option<usize>,
}

impl IndexedFile {
    pub(super) fn new(fingerprint: FileFingerprint) -> Self {
        Self {
            fingerprint,
            line_starts: Vec::new(),
            scanned_to: 0,
            total_lines: None,
        }
    }

    pub(super) fn reset(&mut self, fingerprint: FileFingerprint) {
        *self = Self::new(fingerprint);
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileBufferStore {
    pub(super) files: Arc<RwLock<HashMap<PathBuf, Arc<tokio::sync::Mutex<IndexedFile>>>>>,
}

impl FileBufferStore {
    pub(super) async fn index_for(
        &self,
        path: &Path,
        fingerprint: FileFingerprint,
    ) -> Arc<tokio::sync::Mutex<IndexedFile>> {
        if let Some(index) = self.files.read().await.get(path).cloned() {
            return index;
        }

        self.files
            .write()
            .await
            .entry(path.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(IndexedFile::new(fingerprint))))
            .clone()
    }

    pub(super) async fn invalidate(&self, path: &Path) {
        let mut paths = vec![absolute_path(path)];
        if let Ok(canonical_path) = fs::canonicalize(path).await {
            paths.push(canonical_path);
        }

        let mut files = self.files.write().await;
        for path in paths {
            files.remove(&path);
        }
    }
}

pub(super) fn is_cacheable(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_file() && metadata.len() > 0
}

pub(super) fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(path))
            .unwrap_or_else(|_| path.to_owned())
    }
}
