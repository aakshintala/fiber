//! Completions written while the session was gone (`docs/events.md`, "Resume").

use std::collections::HashSet;
use std::io::{Read as _, Seek as _, SeekFrom};

use contract::events::{Event, EventKind, InputItem, JobCompleted, Outcome};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{Envelope, ErrorCode};

use crate::{Error, Loop};

pub(crate) fn collect(lines: &[Envelope]) -> Result<Vec<JobCompleted>, Error> {
    let mut owned = HashSet::new();
    let mut kept = Vec::new();
    for line in lines {
        let kind = EventKind::from_name(&line.kind);
        if kind == EventKind::JobStarted
            && line
                .payload
                .get("extension")
                .is_some_and(|value| !value.is_null())
            && let Some(id) = line.payload.get("job_id").and_then(|value| value.as_str())
        {
            owned.insert(id.to_owned());
        } else if matches!(
            kind,
            EventKind::TurnStarted | EventKind::FiberExited | EventKind::Rewound | EventKind::Moved
        ) {
            kept.clear();
        } else if kind == EventKind::JobCompleted
            && line.turn_id.is_none()
            && ["output_tail", "notice", "changed_by"]
                .iter()
                .all(|key| !line.payload.contains_key(*key))
            && let Some(Event::JobCompleted(completed)) =
                Event::from_envelope(line).map_err(Error::Unreadable)?
            && !owned.contains(&completed.job_id.0)
            && completed
                .error
                .as_ref()
                .is_none_or(|error| error.code != ErrorCode::Orphaned)
        {
            kept.push(completed);
        }
    }
    Ok(kept)
}

impl Loop {
    pub(crate) fn deliver_kept(&mut self) -> Result<Option<InputItem>, Error> {
        let mut notices = Vec::new();
        let mut changed_by = Vec::new();
        for mut completed in std::mem::take(&mut self.kept) {
            if completed.status == Outcome::Failed
                && let Some(started) = self.ending.starts.get(&completed.job_id)
            {
                completed.output_tail = tail(self.log.dir(), &started.output_path);
            }
            let completed = self.deliver_completion(completed, None)?;
            self.redact_output(&completed, None)?;
            if let Some(changed) = &completed.changed_by {
                for extension in changed {
                    if !changed_by.contains(extension) {
                        changed_by.push(extension.clone());
                    }
                }
            }
            notices.push(crate::jobs::notice_text(&completed));
        }
        if notices.is_empty() {
            return Ok(None);
        }
        Ok(Some(InputItem::Message {
            content: vec![ContentPart::Text {
                text: notices.join("\n\n"),
            }],
            sender: Sender {
                origin: Origin::Fiber,
                command_id: None,
            },
            changed_by: (!changed_by.is_empty()).then_some(changed_by),
        }))
    }
}

fn tail(dir: &std::path::Path, recorded: &std::path::Path) -> Option<String> {
    let mut file = log::open_artifact(dir, &recorded.to_string_lossy()).ok()?;
    let size = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(2048))).ok()?;
    let mut bytes = Vec::new();
    file.take(2048).read_to_end(&mut bytes).ok()?;
    let skip = bytes
        .iter()
        .take(3)
        .take_while(|byte| **byte & 0xc0 == 0x80)
        .count();
    let text = String::from_utf8_lossy(bytes.get(skip..).unwrap_or_default()).into_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
#[path = "kept_tests.rs"]
mod tests;
