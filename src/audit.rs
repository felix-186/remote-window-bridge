use crate::{
    audit_index::{DiskIndex, Location},
    error::{DesktopError, Result},
    types::{Action, ActionReceipt, ActionRequest},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Intent {
        action_id: String,
        fingerprint: String,
        request: serde_json::Value,
    },
    Outcome {
        receipt: ActionReceipt,
    },
}
const MAX_RECORD: u64 = 2 * 1024 * 1024;
pub struct Audit {
    file: File,
    actions: HashMap<String, (String, ActionReceipt)>,
    healthy: bool,
    index: DiskIndex,
    cache_order: VecDeque<String>,
    segments: Vec<PathBuf>,
    segment_limit: u64,
}
impl Audit {
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"segment_limit_bytes":self.segment_limit, "segments":self.segments.len(),
            "cached_actions":self.actions.len(), "cache_capacity":512,
            "index":"temporary_disk_index_rebuilt_at_startup", "healthy":self.healthy})
    }
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_segment_limit(path, 16 * 1024 * 1024)
    }
    pub fn open_with_segment_limit(path: &Path, segment_limit: u64) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(io_error)?.join(path)
        };
        let path = absolute.as_path();
        if segment_limit == 0 {
            return Err(DesktopError::invalid(
                "Audit segment limit must be positive",
            ));
        }
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(io_error)?;
        let segments = segment_paths(path)?;
        let mut audit = Self {
            file,
            actions: HashMap::new(),
            healthy: true,
            index: DiskIndex::new()?,
            cache_order: VecDeque::new(),
            segments,
            segment_limit,
        };
        // Stream source logs into a rebuildable disk index: startup memory is bounded too.
        for segment in 0..audit.segments.len() {
            let mut reader =
                BufReader::new(File::open(&audit.segments[segment]).map_err(io_error)?);
            let mut offset = 0;
            loop {
                let mut line = Vec::new();
                let length = reader
                    .by_ref()
                    .take(MAX_RECORD + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(io_error)?;
                if length == 0 {
                    break;
                }
                if length as u64 > MAX_RECORD {
                    return Err(DesktopError::new("AUDIT_CORRUPT", "Oversized audit record"));
                }
                if line.last() != Some(&b'\n') {
                    return Err(DesktopError::new(
                        "AUDIT_CORRUPT",
                        "Incomplete audit tail; preserve all segments before repair",
                    ));
                }
                let event: Event = serde_json::from_slice(&line).map_err(|error| {
                    DesktopError::new(
                        "AUDIT_CORRUPT",
                        format!("Segment {segment}, offset {offset}: {error}"),
                    )
                })?;
                let (id, fingerprint, receipt) = match event {
                    Event::Intent {
                        action_id,
                        fingerprint,
                        ..
                    } => {
                        if audit.index.find(&action_id)?.is_some() {
                            return Err(DesktopError::new(
                                "AUDIT_CORRUPT",
                                "Duplicate action intent",
                            ));
                        }
                        let receipt = uncertain(&action_id);
                        (action_id, fingerprint, receipt)
                    }
                    Event::Outcome { receipt } => {
                        let entry = audit.index.find(&receipt.action_id)?.ok_or_else(|| {
                            DesktopError::new("AUDIT_CORRUPT", "Outcome without intent")
                        })?;
                        (receipt.action_id.clone(), entry.fingerprint, receipt)
                    }
                };
                audit.index.insert(
                    &id,
                    &fingerprint,
                    Location {
                        segment: segment as u64,
                        offset,
                        length: length as u64,
                    },
                )?;
                audit.cache(id, fingerprint, receipt);
                offset += length as u64;
            }
        }
        Ok(audit)
    }
    pub fn fingerprint(request: &ActionRequest) -> Result<String> {
        let bytes =
            serde_json::to_vec(request).map_err(|e| DesktopError::invalid(e.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    pub fn lookup(&self, request: &ActionRequest) -> Result<Option<ActionReceipt>> {
        match self.find(&request.action_id)? {
            None => Ok(None),
            Some((hash, receipt)) if hash == Self::fingerprint(request)? => Ok(Some(receipt)),
            Some(_) => Err(DesktopError::new(
                "ACTION_ID_CONFLICT",
                "action_id already exists with different parameters",
            )),
        }
    }
    pub fn get(&self, id: &str) -> Result<ActionReceipt> {
        self.find(id)?
            .map(|(_, receipt)| receipt)
            .ok_or_else(|| DesktopError::new("ACTION_NOT_FOUND", "Unknown action_id"))
    }
    fn append(&mut self, event: &Event) -> Result<Location> {
        if !self.healthy {
            return Err(DesktopError::new(
                "AUDIT_UNAVAILABLE",
                "Audit writer failed earlier; restart after inspecting/repairing the log",
            ));
        }
        let mut value = serde_json::to_value(event)
            .map_err(|e| DesktopError::new("AUDIT_UNAVAILABLE", e.to_string()))?;
        value["recorded_at_ms"] = serde_json::json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64);
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|e| DesktopError::new("AUDIT_UNAVAILABLE", e.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_RECORD {
            return Err(DesktopError::new(
                "AUDIT_UNAVAILABLE",
                "Audit record exceeds 2 MiB",
            ));
        }
        if self.file.metadata().map_err(io_error)?.len() >= self.segment_limit {
            if let Err(error) = self.rotate() {
                self.healthy = false;
                return Err(error);
            }
        }
        let offset = self.file.metadata().map_err(io_error)?.len();
        if let Err(error) = self
            .file
            .write_all(&bytes)
            .and_then(|_| self.file.sync_data())
        {
            self.healthy = false;
            return Err(io_error(error));
        }
        Ok(Location {
            segment: self.segments.len() as u64 - 1,
            offset,
            length: bytes.len() as u64,
        })
    }
    pub fn intent(&mut self, request: &ActionRequest) -> Result<()> {
        if self.find(&request.action_id)?.is_some() {
            return Err(DesktopError::new(
                "ACTION_ID_CONFLICT",
                "Action intent already exists",
            ));
        }
        let mut value =
            serde_json::to_value(request).map_err(|e| DesktopError::invalid(e.to_string()))?;
        if let Action::TypeText {
            text,
            sensitive: true,
            ..
        }
        | Action::Fill {
            text,
            sensitive: true,
            ..
        } = &request.action
        {
            value["action"]["text"] =
                serde_json::json!(format!("[REDACTED: {} characters]", text.chars().count()));
        }
        let hash = Self::fingerprint(request)?;
        let location = self.append(&Event::Intent {
            action_id: request.action_id.clone(),
            fingerprint: hash.clone(),
            request: value,
        })?;
        self.update(
            &request.action_id,
            &hash,
            location,
            uncertain(&request.action_id),
        )?;
        Ok(())
    }
    pub fn outcome(&mut self, receipt: &ActionReceipt) -> Result<()> {
        let (fingerprint, _) = self.find(&receipt.action_id)?.ok_or_else(|| {
            DesktopError::new("ACTION_NOT_FOUND", "Cannot record outcome without intent")
        })?;
        let location = self.append(&Event::Outcome {
            receipt: receipt.clone(),
        })?;
        self.update(&receipt.action_id, &fingerprint, location, receipt.clone())?;
        Ok(())
    }

    fn cache(&mut self, id: String, fingerprint: String, receipt: ActionReceipt) {
        if !self.actions.contains_key(&id) {
            self.cache_order.push_back(id.clone());
        }
        self.actions.insert(id, (fingerprint, receipt));
        if self.actions.len() > 512 {
            if let Some(oldest) = self.cache_order.pop_front() {
                self.actions.remove(&oldest);
            }
        }
    }
    fn update(
        &mut self,
        id: &str,
        fingerprint: &str,
        location: Location,
        receipt: ActionReceipt,
    ) -> Result<()> {
        if let Err(error) = self.index.insert(id, fingerprint, location) {
            self.healthy = false;
            return Err(error);
        }
        self.cache(id.into(), fingerprint.into(), receipt);
        Ok(())
    }
    fn find(&self, id: &str) -> Result<Option<(String, ActionReceipt)>> {
        if let Some(entry) = self.actions.get(id) {
            return Ok(Some(entry.clone()));
        }
        let Some(entry) = self.index.find(id)? else {
            return Ok(None);
        };
        let path = self
            .segments
            .get(entry.location.segment as usize)
            .ok_or_else(|| DesktopError::new("AUDIT_UNAVAILABLE", "Invalid derived segment"))?;
        let mut file = File::open(path).map_err(io_error)?;
        file.seek(SeekFrom::Start(entry.location.offset))
            .map_err(io_error)?;
        let mut bytes = vec![0; entry.location.length as usize];
        file.read_exact(&mut bytes).map_err(io_error)?;
        let event: Event = serde_json::from_slice(&bytes)
            .map_err(|error| DesktopError::new("AUDIT_CORRUPT", error.to_string()))?;
        let receipt = match event {
            Event::Intent { action_id, .. } if action_id == id => uncertain(id),
            Event::Outcome { receipt } if receipt.action_id == id => receipt,
            _ => return Err(DesktopError::new("AUDIT_CORRUPT", "Index/action mismatch")),
        };
        Ok(Some((entry.fingerprint, receipt)))
    }
    fn rotate(&mut self) -> Result<()> {
        let current = self.segments.last().unwrap().clone();
        let archive = archive_path(&current, self.segments.len());
        if archive.exists() {
            return Err(DesktopError::new(
                "AUDIT_UNAVAILABLE",
                "Audit archive already exists",
            ));
        }
        std::fs::rename(&current, &archive).map_err(io_error)?;
        *self.segments.last_mut().unwrap() = archive;
        self.file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .read(true)
            .open(&current)
            .map_err(io_error)?;
        self.segments.push(current);
        Ok(())
    }
}
fn uncertain(id: &str) -> ActionReceipt {
    ActionReceipt {
        action_id: id.into(),
        execution_status: "uncertain".into(),
        verification_status: "uncertain".into(),
        text_input_mode: None,
        sent_events: 0,
        next_frame_id: None,
        error: Some(DesktopError::new(
            "ACTION_UNCERTAIN",
            "Intent was recorded; execution outcome is unknown. Do not replay automatically.",
        )),
    }
}
fn archive_path(path: &Path, number: usize) -> PathBuf {
    path.with_file_name(format!(
        "{}.segment-{number:020}.jsonl",
        path.file_name().unwrap().to_string_lossy()
    ))
}
fn segment_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let prefix = format!("{}.segment-", path.file_name().unwrap().to_string_lossy());
    let mut archives = Vec::new();
    for entry in std::fs::read_dir(
        path.parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
    .map_err(io_error)?
    {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(number) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".jsonl"))
        {
            if number.len() == 20 && number.bytes().all(|c| c.is_ascii_digit()) {
                if !entry.file_type().map_err(io_error)?.is_file() {
                    return Err(DesktopError::new(
                        "AUDIT_CORRUPT",
                        "Audit segment is not a regular file",
                    ));
                }
                archives.push(entry.path());
            }
        }
    }
    archives.sort();
    for (index, archive) in archives.iter().enumerate() {
        if *archive != archive_path(path, index + 1) {
            return Err(DesktopError::new(
                "AUDIT_CORRUPT",
                "Missing audit segment; preserve full history",
            ));
        }
    }
    archives.push(path.to_path_buf());
    Ok(archives)
}
fn io_error(e: std::io::Error) -> DesktopError {
    DesktopError::new("AUDIT_UNAVAILABLE", e.to_string())
}
