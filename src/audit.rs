use crate::{
    error::{DesktopError, Result},
    types::{Action, ActionReceipt, ActionRequest},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
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
pub struct Audit {
    file: File,
    actions: HashMap<String, (String, ActionReceipt)>,
    healthy: bool,
}
impl Audit {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(io_error)?;
        if file.metadata().map_err(io_error)?.len() > 0 {
            let mut check = file.try_clone().map_err(io_error)?;
            check.seek(SeekFrom::End(-1)).map_err(io_error)?;
            let mut last = [0u8];
            check.read_exact(&mut last).map_err(io_error)?;
            if last[0] != b'\n' {
                return Err(DesktopError::new("AUDIT_CORRUPT", "Incomplete audit tail; preserve the log and inspect uncertain actions before repair"));
            }
            check.seek(SeekFrom::Start(0)).map_err(io_error)?;
        }
        let mut actions = HashMap::new();
        // A truncated/corrupt log fails closed instead of silently forgetting prior inputs.
        for (index, line) in BufReader::new(file.try_clone().map_err(io_error)?)
            .lines()
            .enumerate()
        {
            let line = line.map_err(io_error)?;
            let event: Event = serde_json::from_str(&line).map_err(|e| {
                DesktopError::new("AUDIT_CORRUPT", format!("Line {}: {e}", index + 1))
            })?;
            match event {
                Event::Intent {
                    action_id,
                    fingerprint,
                    ..
                } => {
                    if actions.contains_key(&action_id) {
                        return Err(DesktopError::new(
                            "AUDIT_CORRUPT",
                            "Duplicate action intent",
                        ));
                    }
                    let receipt = ActionReceipt { action_id: action_id.clone(), execution_status: "uncertain".into(),
                        verification_status: "uncertain".into(), sent_events: 0, next_frame_id: None,
                        error: Some(DesktopError::new("ACTION_UNCERTAIN", "Intent was recorded; execution outcome is unknown. Do not replay automatically.")) };
                    actions.insert(action_id, (fingerprint, receipt));
                }
                Event::Outcome { receipt } => {
                    let entry = actions.get_mut(&receipt.action_id).ok_or_else(|| {
                        DesktopError::new("AUDIT_CORRUPT", "Outcome without intent")
                    })?;
                    entry.1 = receipt;
                }
            }
        }
        Ok(Self {
            file,
            actions,
            healthy: true,
        })
    }
    pub fn fingerprint(request: &ActionRequest) -> Result<String> {
        let bytes =
            serde_json::to_vec(request).map_err(|e| DesktopError::invalid(e.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    pub fn lookup(&self, request: &ActionRequest) -> Result<Option<ActionReceipt>> {
        match self.actions.get(&request.action_id) {
            None => Ok(None),
            Some((hash, receipt)) if *hash == Self::fingerprint(request)? => {
                Ok(Some(receipt.clone()))
            }
            Some(_) => Err(DesktopError::new(
                "ACTION_ID_CONFLICT",
                "action_id already exists with different parameters",
            )),
        }
    }
    pub fn get(&self, id: &str) -> Result<ActionReceipt> {
        self.actions
            .get(id)
            .map(|(_, r)| r.clone())
            .ok_or_else(|| DesktopError::new("ACTION_NOT_FOUND", "Unknown action_id"))
    }
    fn append(&mut self, event: &Event) -> Result<()> {
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
        if let Err(error) = self
            .file
            .write_all(&bytes)
            .and_then(|_| self.file.sync_data())
        {
            self.healthy = false;
            return Err(io_error(error));
        }
        Ok(())
    }
    pub fn intent(&mut self, request: &ActionRequest) -> Result<()> {
        if self.actions.contains_key(&request.action_id) {
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
        } = &request.action
        {
            value["action"]["text"] =
                serde_json::json!(format!("[REDACTED: {} characters]", text.chars().count()));
        }
        let hash = Self::fingerprint(request)?;
        self.append(&Event::Intent {
            action_id: request.action_id.clone(),
            fingerprint: hash.clone(),
            request: value,
        })?;
        self.actions.insert(
            request.action_id.clone(),
            (
                hash,
                ActionReceipt {
                    action_id: request.action_id.clone(),
                    execution_status: "uncertain".into(),
                    verification_status: "uncertain".into(),
                    sent_events: 0,
                    next_frame_id: None,
                    error: Some(DesktopError::new(
                        "ACTION_UNCERTAIN",
                        "Execution may have started",
                    )),
                },
            ),
        );
        Ok(())
    }
    pub fn outcome(&mut self, receipt: &ActionReceipt) -> Result<()> {
        if !self.actions.contains_key(&receipt.action_id) {
            return Err(DesktopError::new(
                "ACTION_NOT_FOUND",
                "Cannot record outcome without intent",
            ));
        }
        self.append(&Event::Outcome {
            receipt: receipt.clone(),
        })?;
        if let Some(entry) = self.actions.get_mut(&receipt.action_id) {
            entry.1 = receipt.clone();
        }
        Ok(())
    }
}
fn io_error(e: std::io::Error) -> DesktopError {
    DesktopError::new("AUDIT_UNAVAILABLE", e.to_string())
}
