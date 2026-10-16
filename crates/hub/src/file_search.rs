//! One cancellable search worker per connection (`docs/invocation.md`).
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::connection::{Hub, Refused};
use crate::feed::{answer, invalid};
use contract::{CommandId, ErrorCode};
use serde_json::{Map, Value, json};

struct Job {
    id: CommandId,
    workspace: PathBuf,
    text: String,
    fresh: bool,
    generation: u64,
}

#[derive(Default)]
pub(crate) struct Search {
    jobs: Option<mpsc::Sender<Job>>,
    newest: Arc<AtomicU64>,
}

impl Search {
    pub(crate) fn submit(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Arc<Hub>,
        writer: &Arc<Mutex<UnixStream>>,
    ) {
        let result = self.enqueue(id, args, hub, writer);
        if let Err(error) = result {
            answer(writer, hub, id, Err(error));
        }
    }
    fn enqueue(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Arc<Hub>,
        writer: &Arc<Mutex<UnixStream>>,
    ) -> Result<(), Refused> {
        if args
            .keys()
            .any(|key| !matches!(key.as_str(), "workspace" | "text" | "fresh"))
        {
            return Err(invalid());
        }
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let workspace = PathBuf::from(workspace);
        if !workspace.is_absolute() {
            return Err(invalid());
        }
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        let fresh = match args.get("fresh") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(invalid)?,
        };
        if self.jobs.is_none() {
            let (out, inbox) = mpsc::channel();
            let h = hub.clone();
            let w = writer.clone();
            let newest = self.newest.clone();
            crate::connection::spawn(hub, "hub-search", move || work(inbox, newest, h, w))
                .map_err(|error| Refused {
                    code: ErrorCode::IoFailed,
                    message: format!("The search could not start: {error}."),
                })?;
            self.jobs = Some(out);
        }
        let generation = self.newest.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        let job = Job {
            id: id.clone(),
            workspace,
            text,
            fresh,
            generation,
        };
        self.jobs
            .as_ref()
            .ok_or_else(invalid)?
            .send(job)
            .map_err(|_| Refused {
                code: ErrorCode::IoFailed,
                message: "The search worker stopped.".to_owned(),
            })
    }
}
impl Drop for Search {
    fn drop(&mut self) {
        self.newest.store(u64::MAX, Ordering::SeqCst);
    }
}

fn work(
    inbox: mpsc::Receiver<Job>,
    newest: Arc<AtomicU64>,
    hub: Arc<Hub>,
    writer: Arc<Mutex<UnixStream>>,
) {
    let mut held: Option<(PathBuf, Box<dyn crate::FileSearch>)> = None;
    let Some(machine) = hub.machine.as_ref() else {
        return;
    };
    for job in inbox {
        if job.fresh || held.as_ref().map(|(workspace, _)| workspace) != Some(&job.workspace) {
            held = Some((
                job.workspace.clone(),
                machine.file_search(&job.workspace, hub.environments.user_home().as_deref()),
            ));
        }
        let cancelled = || newest.load(Ordering::SeqCst) != job.generation;
        let result = if cancelled() {
            None
        } else {
            held.as_mut()
                .and_then(|(_, search)| search.search(&job.text, &cancelled))
        };
        let result = if cancelled() { None } else { result };
        let result = match result {
            None => Err(Refused {
                code: ErrorCode::StaleRequest,
                message: "A newer search replaced this one.".into(),
            }),
            Some(Ok(rows)) => Ok(Some(json!({"rows":rows}))),
            Some(Err(error)) => Ok(Some(json!({"error":error}))),
        };
        answer(&writer, &hub, &job.id, result);
    }
}

#[cfg(test)]
#[path = "file_search_tests.rs"]
pub(crate) mod tests;
