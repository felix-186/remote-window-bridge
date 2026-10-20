//! Screenshot retention is independent of durable action history.
use crate::error::{DesktopError, Result};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Clone, Serialize)]
pub struct FrameRetention {
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_age_secs: u64,
    /// Never remove recently returned screenshots while a client may still read them.
    pub grace_secs: u64,
}
impl Default for FrameRetention {
    fn default() -> Self {
        Self {
            max_files: 256,
            max_bytes: 256 * 1024 * 1024,
            max_age_secs: 24 * 60 * 60,
            grace_secs: 120,
        }
    }
}
pub struct FrameStore {
    directory: PathBuf,
    pub retention: FrameRetention,
    publications: u64,
    pub cleanup_error: Option<String>,
}
impl FrameStore {
    pub fn new(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory).map_err(storage_error)?;
        Ok(Self {
            directory,
            retention: FrameRetention::default(),
            publications: 0,
            cleanup_error: None,
        })
    }
    pub fn path(&self, frame_id: &str) -> PathBuf {
        self.directory.join(format!("{frame_id}.png"))
    }
    pub fn write(&mut self, path: &Path, png: &[u8]) -> Result<()> {
        if path.parent() != Some(self.directory.as_path()) || !owned_frame(path) {
            return Err(DesktopError::invalid("Invalid screenshot storage path"));
        }
        // Frame IDs include process/instance/sequence and are never reused.
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(storage_error)?;
        file.write_all(png).map_err(storage_error)?;
        self.publications += 1;
        if self.publications == 1 || self.publications % 32 == 0 {
            self.cleanup_error = self.prune(path).err().map(|error| error.to_string());
        }
        Ok(())
    }
    pub fn prune(&self, keep: &Path) -> Result<()> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(storage_error)? {
            let entry = entry.map_err(storage_error)?;
            if !entry.file_type().map_err(storage_error)?.is_file() || !owned_frame(&entry.path()) {
                continue;
            }
            let metadata = entry.metadata().map_err(storage_error)?;
            files.push((
                entry.path(),
                metadata.modified().map_err(storage_error)?,
                metadata.len(),
            ));
        }
        files.sort_by_key(|(_, modified, _)| *modified);
        let mut count = files.len();
        let mut bytes: u64 = files.iter().map(|(_, _, size)| size).sum();
        let now = SystemTime::now();
        for (path, modified, size) in files {
            let age = now
                .duration_since(modified)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            if path == keep || age < self.retention.grace_secs {
                continue;
            }
            if count > self.retention.max_files
                || bytes > self.retention.max_bytes
                || age > self.retention.max_age_secs
            {
                // Only direct, regular, bridge-named PNG files in this dedicated directory.
                fs::remove_file(&path).map_err(storage_error)?;
                count -= 1;
                bytes = bytes.saturating_sub(size);
            }
        }
        Ok(())
    }
}
fn owned_frame(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.strip_prefix("frame-")
                .and_then(|name| name.strip_suffix(".png"))
                .is_some_and(|id| {
                    !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit() || c == b'-')
                })
        })
}
fn storage_error(error: std::io::Error) -> DesktopError {
    DesktopError::new("STORAGE_UNAVAILABLE", error.to_string())
}
