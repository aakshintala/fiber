//! Session store: keeps the last N tool calls per session for `/usage`.
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CALLS: usize = 64;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub ms: u64,
    pub ok: bool,
}

#[derive(Debug, Default)]
pub struct SessionStore {
    calls: VecDeque<ToolCall>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            calls: VecDeque::with_capacity(MAX_CALLS),
        }
    }

    pub fn push(&mut self, name: &str, ms: u64, ok: bool) {
        if self.calls.len() == MAX_CALLS {
            self.calls.pop_front();
        }
        self.calls.push_back(ToolCall {
            name: name.to_string(),
            ms,
            ok,
        });
    }

    pub fn error_rate(&self) -> f64 {
        let total = self.calls.len();
        if total == 0 {
            return 0.0;
        }
        let errs = self.calls.iter().filter(|c| !c.ok).count();
        errs as f64 / total as f64
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
