//! Bounded, payload-free timings. Stage durations can overlap (for example dispatch/capture).
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone, Default, Serialize)]
struct Report {
    operation: String,
    stages_us: BTreeMap<String, u64>,
    calls: BTreeMap<String, u64>,
}
#[derive(Default)]
struct State {
    current: Report,
    recent: VecDeque<Report>,
}
#[derive(Default)]
pub struct Performance(Mutex<State>);
impl Performance {
    pub fn begin(&self, operation: &str, queue_us: u64) {
        self.0.lock().unwrap().current = Report {
            operation: operation.into(),
            stages_us: BTreeMap::from([("queue".into(), queue_us)]),
            ..Report::default()
        };
    }
    pub fn finish(&self) {
        let mut state = self.0.lock().unwrap();
        let report = std::mem::take(&mut state.current);
        state.recent.push_back(report);
        if state.recent.len() > 32 {
            state.recent.pop_front();
        }
    }
    pub fn stage(self: &Arc<Self>, name: &'static str) -> Stage {
        Stage {
            metrics: self.clone(),
            name,
            start: Instant::now(),
        }
    }
    pub fn snapshot(&self) -> serde_json::Value {
        let state = self.0.lock().unwrap();
        serde_json::json!({"unit":"microseconds", "stages_overlap":true,
            "current":state.current, "recent":state.recent})
    }
}
pub struct Stage {
    metrics: Arc<Performance>,
    name: &'static str,
    start: Instant,
}
impl Drop for Stage {
    fn drop(&mut self) {
        let mut state = self.metrics.0.lock().unwrap();
        *state.current.stages_us.entry(self.name.into()).or_default() +=
            self.start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        *state.current.calls.entry(self.name.into()).or_default() += 1;
    }
}
