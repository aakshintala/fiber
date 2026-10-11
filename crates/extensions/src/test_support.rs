//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::sync::atomic::{AtomicUsize, Ordering};

use contract::events::CacheLifetime;
use contract::provider::{
    CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Provider, Reply,
    ToolDefinition,
};
use serde_json::{Map, Value, json};

pub(crate) use fakes::write_record;

/// A provider whose tools are its own shape, which counts its calls.
#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) calls: AtomicUsize,
    pub(crate) warms: bool,
}

pub(crate) struct Idle;

impl ModelCall for Idle {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        Err(CallError::Cancelled {
            usage: Box::new(CallUsage::unnamed(InputSize::default())),
        })
    }

    fn cancel(&self) {}
}

impl Provider for Inner {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::new(Idle)
    }

    fn warms(&self, _request: &ModelRequest) -> bool {
        self.warms
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        tools
            .iter()
            .map(|tool| {
                let mut map = Map::new();
                map.insert("own".into(), json!(tool.name));
                map
            })
            .collect()
    }
}

pub(crate) fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: Vec::new(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}
