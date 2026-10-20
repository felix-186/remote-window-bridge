//! Rebuildable disk index. Logs remain the durable source of truth.
//! Fixed memory: 256 bucket heads and a 128 KiB negative-membership filter.
use crate::error::{DesktopError, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy)]
pub(crate) struct Location {
    pub segment: u64,
    pub offset: u64,
    pub length: u64,
}
pub(crate) struct Entry {
    pub fingerprint: String,
    pub location: Location,
}
pub(crate) struct DiskIndex {
    file: File,
    heads: [u64; 256],
    bloom: Vec<u8>,
}
const RECORD: usize = 128;
impl DiskIndex {
    pub fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Temporary derived data disappears even if this process crashes.
            options.custom_flags(0x04000000 | 0x00000100); // DELETE_ON_CLOSE | TEMPORARY
        }
        let path = std::env::temp_dir().join(format!(
            "remote-window-bridge-index-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = options.open(&path).map_err(io_error)?;
        #[cfg(not(windows))]
        std::fs::remove_file(path).map_err(io_error)?;
        // Offset zero is the end-of-chain sentinel.
        file.write_all(&[0]).map_err(io_error)?;
        Ok(Self {
            file,
            heads: [0; 256],
            bloom: vec![0; 128 * 1024],
        })
    }
    fn bits(hash: &[u8]) -> impl Iterator<Item = usize> + '_ {
        hash[..12]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize % (128 * 1024 * 8))
    }
    pub fn find(&self, id: &str) -> Result<Option<Entry>> {
        let hash = Sha256::digest(id.as_bytes());
        if Self::bits(&hash).any(|bit| self.bloom[bit / 8] & (1 << (bit % 8)) == 0) {
            return Ok(None);
        }
        let mut cursor = self.heads[hash[0] as usize];
        let mut file = self.file.try_clone().map_err(io_error)?;
        let mut bytes = [0; RECORD];
        while cursor != 0 {
            file.seek(SeekFrom::Start(cursor)).map_err(io_error)?;
            file.read_exact(&mut bytes).map_err(io_error)?;
            if bytes[8..40] == hash[..] {
                let number =
                    |start| u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap());
                return Ok(Some(Entry {
                    fingerprint: String::from_utf8(bytes[40..104].to_vec()).map_err(|_| {
                        DesktopError::new("AUDIT_UNAVAILABLE", "Invalid derived fingerprint")
                    })?,
                    location: Location {
                        segment: number(104),
                        offset: number(112),
                        length: number(120),
                    },
                }));
            }
            let previous = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            if previous >= cursor {
                return Err(DesktopError::new(
                    "AUDIT_UNAVAILABLE",
                    "Invalid derived index chain",
                ));
            }
            cursor = previous;
        }
        Ok(None)
    }
    pub fn insert(&mut self, id: &str, fingerprint: &str, location: Location) -> Result<()> {
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(DesktopError::new(
                "AUDIT_CORRUPT",
                "Invalid action fingerprint",
            ));
        }
        let hash = Sha256::digest(id.as_bytes());
        let bucket = hash[0] as usize;
        let mut bytes = Vec::with_capacity(RECORD);
        bytes.extend(self.heads[bucket].to_le_bytes());
        bytes.extend(hash);
        bytes.extend(fingerprint.as_bytes());
        bytes.extend(location.segment.to_le_bytes());
        bytes.extend(location.offset.to_le_bytes());
        bytes.extend(location.length.to_le_bytes());
        let offset = self.file.seek(SeekFrom::End(0)).map_err(io_error)?;
        self.file.write_all(&bytes).map_err(io_error)?;
        self.heads[bucket] = offset;
        for bit in Self::bits(&hash) {
            self.bloom[bit / 8] |= 1 << (bit % 8);
        }
        Ok(())
    }
}
fn io_error(error: std::io::Error) -> DesktopError {
    DesktopError::new("AUDIT_UNAVAILABLE", error.to_string())
}
