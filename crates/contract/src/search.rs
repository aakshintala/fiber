//! An installed search backend Fiber's own `web_search` runs
//! (`docs/tools.md`, "Fiber's own, over a backend").

use crate::shapes::Failure;
use crate::tool::Cancel;
use serde::Deserialize;

/// Which domains a search keeps (`docs/tools.md`, "web_search").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domains {
    /// No domain filter.
    Any,
    /// Only these domains.
    Allowed(Vec<String>),
    /// Every domain but these.
    Blocked(Vec<String>),
}

/// One result a search backend returned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResult {
    /// The result's title.
    pub title: String,
    /// The result's URL.
    pub url: String,
    /// The result's snippet.
    pub snippet: String,
}

/// An installed search backend Fiber's own `web_search` runs
/// (`docs/tools.md`, "Fiber's own, over a backend").
pub trait SearchBackend: Send + Sync {
    /// The results for `query` under `domains`, in the backend's order;
    /// `Ok(None)` once `cancel` stopped the search.
    fn search(
        &self,
        query: &str,
        domains: &Domains,
        cancel: &dyn Cancel,
    ) -> Result<Option<Vec<SearchResult>>, Failure>;
}
