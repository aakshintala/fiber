//! A provider's `cost()` (`docs/model-routing.md`, "Cost"): the lookup of a
//! generation's cost for a call that ended without the vendor's own figure,
//! and the provider wrapper that carries it to the loop.

use std::sync::Arc;

use config::Secret;
use contract::GenerationId;
use contract::provider::{CostLookup, ModelCall, ModelRequest, Provider, ToolDefinition};
use serde_json::{Map, Value, json};

use crate::{Error, LuaProvider};

impl LuaProvider {
    /// Calls `cost({ generation_id, base_url, key })` and returns the cost it
    /// returned, in US dollars: `None` when it returned nothing. `base_url`
    /// is the call's model's, so the lookup goes to the host the call went
    /// to; `key` is absent when the session has none for the provider. A
    /// finite number at or above 0 is a cost; any other return is an error.
    pub fn cost(
        &self,
        generation_id: &GenerationId,
        base_url: &str,
        key: Option<&Secret>,
    ) -> Result<Option<f64>, Error> {
        let mut arg = Map::new();
        arg.insert("generation_id".into(), json!(generation_id.0));
        arg.insert("base_url".into(), json!(base_url));
        if let Some(key) = key {
            arg.insert("key".into(), json!(key.expose()));
        }
        let returned = self.call("cost", Value::Object(arg))?;
        match returned {
            Value::Null => Ok(None),
            Value::Number(n) => match n.as_f64() {
                Some(cost) if cost.is_finite() && cost >= 0.0 => Ok(Some(cost)),
                Some(_) | None => Err(self.not_a_cost()),
            },
            Value::Bool(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
                Err(self.not_a_cost())
            }
        }
    }

    fn not_a_cost(&self) -> Error {
        self.bad_return(
            "cost",
            "something other than a cost in US dollars at or above 0".into(),
        )
    }

    /// `inner`, the provider a session's calls go through, with this
    /// provider's `cost()` as its lookup; `inner` itself when this provider
    /// did not register `cost`. The lookup uses `base_url` and `key`, the
    /// ones `inner` was built with.
    pub fn costed(
        self: &Arc<Self>,
        inner: Arc<dyn Provider>,
        base_url: &str,
        key: Option<Secret>,
    ) -> Result<Arc<dyn Provider>, Error> {
        if !self.registers("cost")? {
            return Ok(inner);
        }
        Ok(Arc::new(Costed {
            inner,
            lookup: Arc::new(LuaCost {
                provider: Arc::clone(self),
                base_url: base_url.to_owned(),
                key,
            }),
        }))
    }
}

/// A provider whose calls are another's, with a `cost()` lookup. Every
/// [`Provider`] method but `cost_lookup` is the inner provider's, so the
/// request bytes are unchanged (`docs/prompt-cache.md`).
struct Costed {
    inner: Arc<dyn Provider>,
    lookup: Arc<LuaCost>,
}

impl Provider for Costed {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.inner.call(request)
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        self.inner.wire_tools(tools)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn CostLookup>> {
        Some(Arc::clone(&self.lookup) as Arc<dyn CostLookup>)
    }
}

/// One provider's `cost()`, bound to the base URL and key its calls used.
struct LuaCost {
    provider: Arc<LuaProvider>,
    base_url: String,
    key: Option<Secret>,
}

impl CostLookup for LuaCost {
    /// An error, a bad return and a passed timeout are all nothing: the
    /// call's cost stays as first recorded (`docs/model-routing.md`, "Cost").
    fn cost(&self, generation_id: &GenerationId) -> Option<f64> {
        self.provider
            .cost(generation_id, &self.base_url, self.key.as_ref())
            .ok()
            .flatten()
    }
}

#[cfg(test)]
#[path = "lua_cost_tests.rs"]
mod tests;
