use crate::error::{DesktopError, Result};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    Arc, Mutex,
};

/// Packed generation and reason make pause/resume atomic as a single state change.
#[derive(Default)]
pub struct Control {
    state: AtomicU64,
    shutdown: AtomicU8,
    request_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

impl Control {
    fn change(&self, reason: u64, agent_only: bool) -> bool {
        let mut old = self.state.load(Ordering::SeqCst);
        loop {
            if agent_only && old & 3 == 2 {
                return false;
            }
            let next = (((old >> 2) + 1) << 2) | reason;
            match self
                .state
                .compare_exchange(old, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(value) => old = value,
            }
        }
    }
    pub fn pause_agent(&self) {
        self.change(1, true);
    }
    pub fn pause_user(&self) {
        self.change(2, false);
    }
    pub fn resume_user(&self) {
        self.change(0, false);
    }
    pub fn resume_agent(&self) -> Result<()> {
        if !self.change(0, true) {
            return Err(DesktopError::new(
                "PAUSED_BY_USER",
                "Use Ctrl+Alt+F11 on the Windows host to resume",
            ));
        }
        Ok(())
    }
    pub fn cancel_current(&self) {
        self.state.fetch_add(4, Ordering::SeqCst);
    }
    pub fn shutdown(&self) {
        self.shutdown.store(1, Ordering::SeqCst);
        self.cancel_current();
    }
    pub fn generation(&self) -> u64 {
        self.state.load(Ordering::SeqCst) >> 2
    }
    pub fn reason(&self) -> &'static str {
        match self.state.load(Ordering::SeqCst) & 3 {
            1 => "agent",
            2 => "user",
            _ => "running",
        }
    }
    pub fn token(self: &Arc<Self>, allow_paused: bool) -> Cancellation {
        Cancellation {
            control: self.clone(),
            generation: self.generation(),
            allow_paused,
            request_cancel: self.request_cancel.lock().unwrap().clone(),
        }
    }
    pub(crate) fn request_scope(self: &Arc<Self>, cancelled: Arc<AtomicBool>) -> RequestScope {
        let previous = self.request_cancel.lock().unwrap().replace(cancelled);
        RequestScope {
            control: self.clone(),
            previous,
        }
    }
}

pub(crate) struct RequestScope {
    control: Arc<Control>,
    previous: Option<Arc<AtomicBool>>,
}
impl Drop for RequestScope {
    fn drop(&mut self) {
        *self.control.request_cancel.lock().unwrap() = self.previous.take();
    }
}

#[derive(Clone)]
pub struct Cancellation {
    control: Arc<Control>,
    generation: u64,
    allow_paused: bool,
    request_cancel: Option<Arc<AtomicBool>>,
}
impl Cancellation {
    /// Observation may run while paused, but must not activate windows or send keys.
    pub fn check_running(&self) -> Result<()> {
        self.check()?;
        if self.control.reason() != "running" {
            return Err(DesktopError::new(
                "PAUSED",
                "Window activation is disabled while paused",
            ));
        }
        Ok(())
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn check(&self) -> Result<()> {
        if self.control.shutdown.load(Ordering::SeqCst) != 0 {
            return Err(DesktopError::new(
                "SHUTTING_DOWN",
                "Agent connection closed",
            ));
        }
        if self
            .request_cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            return Err(DesktopError::new("CANCELLED", "MCP request was cancelled"));
        }
        if !self.allow_paused && self.control.reason() != "running" {
            return Err(DesktopError::new(
                "PAUSED",
                format!("Paused by {}", self.control.reason()),
            ));
        }
        if self.control.generation() != self.generation {
            return Err(DesktopError::new(
                "CANCELLED",
                "Operation invalidated by cancellation or user takeover",
            ));
        }
        Ok(())
    }
}
