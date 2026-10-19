use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, DesktopError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_events: Option<u32>,
}

impl DesktopError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            sent_events: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message)
    }
    pub fn with_sent(mut self, sent: u32) -> Self {
        self.sent_events = Some(sent);
        self
    }
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for DesktopError {}
