//! The flake-report decision (`docs/testing/running.md`, "Flaky tests"):
//! which closed flake issue a failure reopens, or that it files nothing.
//! It reads no API and runs no Git: the workflow passes each lookup with an
//! explicit completeness status, and a failed command, invalid shape or
//! incomplete pagination arrives as failed/incomplete, never as an empty
//! list. `main.rs` is its only caller.

use serde_json::{Map, Value};

/// One JSON-lines decision: the unique flake title, the action the writer
/// takes, and the target issue number where one applies.
pub(crate) struct Decision {
    title: String,
    action: &'static str,
    issue: Option<u64>,
}

impl Decision {
    pub(crate) fn line(&self) -> String {
        let mut map = Map::new();
        map.insert("action".to_owned(), Value::String(self.action.to_owned()));
        if let Some(issue) = self.issue {
            map.insert("issue".to_owned(), Value::Number(issue.into()));
        }
        map.insert("title".to_owned(), Value::String(self.title.clone()));
        Value::Object(map).to_string()
    }
}

/// The JSON-lines decisions for every unique flake title, in first-seen
/// order. A complete open exact-title match emits nothing: the writer
/// comments on it. Every other title emits exactly one line.
pub(crate) fn decide(input: &Value) -> Result<Vec<Decision>, String> {
    let root = object(input, "input")?;
    let flakes = array(field(root, "input", "flakes")?, "flakes")?;
    let mut titles = Vec::new();
    for flake in flakes {
        let title = text(object(flake, "flakes")?, "flakes", "title")?;
        if !titles.iter().any(|seen| seen == title) {
            titles.push(title.to_owned());
        }
    }
    let open = lookup(field(root, "input", "open")?, "open")?;
    let closed = lookup(field(root, "input", "closed")?, "closed")?;
    let events = object(field(root, "input", "events")?, "events")?;
    let ancestry = object(field(root, "input", "ancestry")?, "ancestry")?;
    let mut out = Vec::new();
    for title in &titles {
        out.extend(title_decision(title, &open, &closed, events, ancestry)?);
    }
    Ok(out)
}

struct Issue {
    number: u64,
    title: String,
    closed_at: String,
    is_pr: bool,
}

struct Lookup {
    ok: bool,
    issues: Vec<Issue>,
}

struct Event {
    is_closed: bool,
    id: u64,
    commit_id: Option<String>,
    created_at: String,
}

fn title_decision(
    title: &str,
    open: &Lookup,
    closed: &Lookup,
    events: &Map<String, Value>,
    ancestry: &Map<String, Value>,
) -> Result<Option<Decision>, String> {
    if !open.ok {
        return Ok(Some(write(title, None, "unknown")));
    }
    if open
        .issues
        .iter()
        .any(|issue| !issue.is_pr && issue.title == title)
    {
        return Ok(None);
    }
    if !closed.ok {
        return Ok(Some(write(title, None, "unknown")));
    }
    let selected = closed
        .issues
        .iter()
        .filter(|issue| !issue.is_pr && issue.title == title)
        .max_by(|a, b| (&a.closed_at, a.number).cmp(&(&b.closed_at, b.number)));
    let Some(selected) = selected else {
        return Ok(Some(write(title, None, "file-new")));
    };
    let key = selected.number.to_string();
    let Some(found) = events.get(&key) else {
        return Ok(Some(write(title, Some(selected.number), "unknown")));
    };
    let lookup = event_lookup(found, &key)?;
    if !lookup.0 {
        return Ok(Some(write(title, Some(selected.number), "unknown")));
    }
    let latest = lookup
        .1
        .iter()
        .filter(|event| event.is_closed)
        .max_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
    let Some(latest) = latest else {
        return Ok(Some(write(title, Some(selected.number), "unknown")));
    };
    let Some(fix) = latest.commit_id.as_deref() else {
        return Ok(Some(write(title, Some(selected.number), "reopen")));
    };
    match ancestry.get(fix) {
        Some(Value::Bool(true)) => Ok(Some(write(title, Some(selected.number), "reopen"))),
        Some(Value::Bool(false)) => Ok(Some(write(title, Some(selected.number), "stale"))),
        _ => Ok(Some(write(title, Some(selected.number), "unknown"))),
    }
}

fn write(title: &str, issue: Option<u64>, action: &'static str) -> Decision {
    Decision {
        title: title.to_owned(),
        action,
        issue,
    }
}

fn lookup(input: &Value, name: &str) -> Result<Lookup, String> {
    let root = object(input, name)?;
    let ok = flag(root, name, "ok")?;
    let listed = array(field(root, name, "issues")?, "issues")?;
    let mut issues = Vec::new();
    for item in listed {
        let issue = object(item, "issues")?;
        issues.push(Issue {
            number: id(field(issue, "issues", "number")?, "number")?,
            title: text(issue, "issues", "title")?.to_owned(),
            closed_at: maybe_text(issue, "closed_at")?
                .unwrap_or_default()
                .to_owned(),
            is_pr: issue
                .get("pull_request")
                .is_some_and(|pr| pr != &Value::Bool(false)),
        });
    }
    Ok(Lookup { ok, issues })
}

fn event_lookup(input: &Value, key: &str) -> Result<(bool, Vec<Event>), String> {
    let root = object(input, &format!("events[{key}]"))?;
    let ok = flag(root, &format!("events[{key}]"), "ok")?;
    let listed = array(field(root, &format!("events[{key}]"), "events")?, "events")?;
    let mut out = Vec::new();
    for item in listed {
        let event = object(item, "events")?;
        out.push(Event {
            is_closed: text(event, "events", "event")? == "closed",
            id: id(field(event, "events", "id")?, "id")?,
            commit_id: match event.get("commit_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(fix)) => Some(fix.clone()),
                Some(_) => {
                    return Err("events: commit_id is not a string or null".to_owned());
                }
            },
            created_at: maybe_text(event, "created_at")?
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok((ok, out))
}

fn object<'a>(input: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    if let Value::Object(map) = input {
        Ok(map)
    } else {
        Err(format!("flake-report: {name} is not an object"))
    }
}

fn array<'a>(input: &'a Value, name: &str) -> Result<&'a Vec<Value>, String> {
    if let Value::Array(items) = input {
        Ok(items)
    } else {
        Err(format!("flake-report: {name} is not an array"))
    }
}

fn field<'a>(root: &'a Map<String, Value>, name: &str, key: &str) -> Result<&'a Value, String> {
    root.get(key)
        .ok_or_else(|| format!("flake-report: {name} has no {key}"))
}

fn text<'a>(root: &'a Map<String, Value>, name: &str, key: &str) -> Result<&'a str, String> {
    if let Value::String(text) = field(root, name, key)? {
        Ok(text)
    } else {
        Err(format!("flake-report: {name} {key} is not a string"))
    }
}

fn maybe_text<'a>(root: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match root.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("flake-report: {key} is not a string or null")),
    }
}

fn flag(root: &Map<String, Value>, name: &str, key: &str) -> Result<bool, String> {
    if let Value::Bool(ok) = field(root, name, key)? {
        Ok(*ok)
    } else {
        Err(format!("flake-report: {name} {key} is not a boolean"))
    }
}

fn id(input: &Value, name: &str) -> Result<u64, String> {
    if let Value::Number(number) = input {
        number
            .as_u64()
            .ok_or_else(|| format!("flake-report: {name} is not a positive integer"))
    } else {
        Err(format!("flake-report: {name} is not a positive integer"))
    }
}

#[cfg(test)]
#[path = "flake_report_tests.rs"]
mod tests;
