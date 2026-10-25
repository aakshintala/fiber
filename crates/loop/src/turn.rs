//! Starting a turn (`docs/loop.md`, "Starting a turn"): the session's run
//! of turns, the one preamble build with the opening message, and the
//! instruction-file and date check, each written before `turn_started`.

use std::collections::BTreeSet;
use std::sync::Arc;

use contract::TurnId;
use contract::events::{
    Event, ToolCallCompleted, ToolInfo, ToolSource, ToolState, TurnOutcome, TurnStarted,
};
use contract::provider::Input;

use crate::{Error, Loop, Preamble, Summary, cancel, inbox, mint, prompt, status};

impl Loop {
    /// Runs turns until `close` is taken or every sender of the inbox is
    /// gone, then, while jobs still run, the ending notice and their ends
    /// (`docs/invocation.md`, "Lifecycle"; `docs/tools.md`, "Background jobs").
    pub fn run(mut self) -> crate::Ran {
        let status = status::spawn(&self);
        self.status_tap = status.as_ref().map(status::Status::tap);
        let result = loop {
            match self.turn() {
                Ok(Some(_)) => {}
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        let result = result.and_then(|()| self.write_last_settled());
        let result = self.settled(result);
        // `fiber_exited` is the last line a process writes: no status
        // follows it.
        if let Some(status) = status {
            status.stop();
        }
        let summary = std::mem::take(&mut self.summary);
        crate::Ran { summary, result }
    }

    /// This process's lines so far, folded as they were written: what
    /// `fiber_exited` will report. For tests that run turns by hand, before
    /// `run` returns.
    pub fn summary(&self) -> &Summary {
        &self.summary
    }

    /// Blocks until a prompt or a steer arrives, then runs one turn from
    /// everything waiting in the inbox, in arrival order (`docs/loop.md`,
    /// "Starting a turn"). Every prompt in that drain joins the turn.
    /// Returns how the turn ended, or `None` once `close` was taken while
    /// idle or every sender of the inbox is gone.
    pub fn turn(&mut self) -> Result<Option<TurnOutcome>, Error> {
        self.apply_switches()?;
        // A shutdown starts no turn and finishes none.
        if self.shutting_down() {
            return Ok(None);
        }
        if let Some(pending) = self.suspended.take() {
            return self.finish_suspended(pending);
        }
        let Some(started) = self.wait_for_turn()? else {
            return Ok(None);
        };
        self.write_settled()?;
        // The one preamble build, before `turn_started`: `answerable` is
        // already what the builder set, so an unattended loop's prompt
        // carries its line. A loop that never takes a turn writes none.
        // The instruction files and the date are checked at each turn
        // start, before `turn_started` — never on the turn that wrote the
        // opening message, whose state was just built from it.
        if !self.ensure_preamble()? {
            self.check_changes()?;
        }
        // A process's first turn runs `session_start` after the preamble
        // and the opening message, before the first model request: a
        // prompt `before_message` refused never reaches here, so it runs
        // no hook, and a process that never takes a turn runs none. A
        // `blocking` failure drops the taken prompts uncalled and the
        // session does not start.
        let start_contexts = self.session_start_once()?;
        let turn = TurnId(mint("t_"));
        self.cut_off = false;
        let (mut input, contexts, state) = self.turn_input(started.pieces);
        let quota = self.quota_due();
        input.extend(
            quota
                .lines
                .into_iter()
                .map(|text| contract::events::InputItem::Message {
                    content: vec![contract::shapes::ContentPart::Text { text }],
                    sender: contract::shapes::Sender {
                        origin: contract::shapes::Origin::Fiber,
                        command_id: None,
                    },
                    changed_by: None,
                }),
        );
        let person = input
            .iter()
            .any(|item| matches!(item, contract::events::InputItem::Handoff { .. }));
        self.handoff.new_turn(person);
        // Armed with `turn_started` and its contexts: a shutdown lands
        // before all of them or after (`docs/extensions.md`, "Hooks").
        let cancel = Arc::clone(&self.cancel);
        let event = Event::TurnStarted(TurnStarted { input });
        let Some(written) = cancel.commit(cancel::Commit::Arm, || -> Result<(), Error> {
            // The hook's contexts go first, with no turn id, so the log
            // reads `opening_message`, `context_added`, `turn_started`;
            // the `before_message` contexts still follow it. A shutdown
            // lands before all of them or after.
            for (added, state) in &start_contexts {
                self.write_carried(state.clone(), None)?;
                self.write(&Event::ContextAdded(added.clone()), None, None)?;
            }
            self.write_carried(state, Some(&turn))?;
            self.append(&event, &turn, None)?;
            for added in &contexts {
                self.write_carried(added.state.clone(), Some(&turn))?;
                self.append(
                    &Event::ContextAdded(contract::events::ContextAdded {
                        text: added.text.clone(),
                        extension: added.extension.clone(),
                        hook: "before_message".into(),
                    }),
                    &turn,
                    None,
                )?;
            }
            for event in &quota.events {
                self.append(event, &turn, None)?;
                // Only a successfully written event changes the session's fold.
                self.quota_notices.fired.fold(event);
            }
            Ok(())
        }) else {
            return Ok(None);
        };
        written?;
        // Each prompt is accepted once its `turn_started` is written, in
        // arrival order. A log error or a shutdown above drops them uncalled.
        for ack in started.prompts {
            inbox::accept(ack);
        }
        // `turn_start` runs after the acknowledgements, before the first
        // model request. A failure ends the turn with no `step_started`.
        if let Event::TurnStarted(started) = &event
            && let Some(completed) = self.turn_start(&turn, &started.input)?
        {
            return self.end_turn(&turn, completed);
        }
        self.run_steps(&turn)
    }

    /// Builds the one preamble and writes `preamble_built`, once per loop.
    /// Later turns reuse what the first turn built: between builds the
    /// preamble does not change (`docs/prompt-cache.md`, "The preamble").
    /// The opening message and its notices follow `preamble_built`, before
    /// `turn_started`.
    pub(crate) fn ensure_preamble(&mut self) -> Result<bool, Error> {
        if self.load_after_reload {
            self.load_after_reload = false;
            let held = self.tools.len();
            let late = self.load_approved()?;
            // A reload's build stands unless a load replaced its tools:
            // a post-offer load that registered one, or the late step.
            if late || self.tools.len() != held {
                self.preamble = None;
            }
        } else if self.preamble.is_some() {
            // A rewound loop holds a logged preamble: the late step
            // installs when this process's offer approved an extension,
            // and no-ops otherwise, so no guard is needed. When it
            // installs, its build replaces the logged one.
            if self.load_approved()? {
                self.preamble = None;
            }
        }
        if self.preamble.is_none() {
            // The post-offer load runs only after the offer step. A reload
            // builds without it and marks that load due for the first turn.
            self.load_approved()?;
            let large = self.construct_preamble()?;
            let opened = self.ensure_opening()?;
            if let Some(notice) = large {
                self.append_early(&Event::Notice(notice))?;
            }
            return Ok(opened);
        }
        self.ensure_opening()
    }

    /// Builds the preamble without writing an opening or running the
    /// post-offer load, so a reload can build before the first turn.
    pub(crate) fn build_preamble(&mut self) -> Result<(), Error> {
        if let Some(notice) = self.construct_preamble()? {
            self.append_early(&Event::Notice(notice))?;
        }
        Ok(())
    }

    fn construct_preamble(&mut self) -> Result<Option<contract::events::Notice>, Error> {
        // The first actual construction in the process owns the one
        // `startup.preamble` scope; a same-session rebuild stays silent.
        // The latch is marked before the build, so a construction that
        // fails after entry still emits exactly once.
        let first = !self.diag.preamble_done();
        self.diag.set_preamble_done();
        let _timer = first
            .then(|| self.diag.timed(contract::diag::Op::StartupPreamble))
            .flatten();
        let unattended = !self.answerable;
        self.set_trigger();
        // Drops the previous build's factory-inserted search tool, if any:
        // the new build reinserts it when due, so a rebuild never takes its
        // own insert for a name the general rule already holds. The name
        // rides the loop, not the preamble, so a switch that discards the
        // preamble still drops it.
        let dropped = self.inserted_search.take();
        if let Some(previous) = &dropped {
            self.tools.remove(previous);
        }
        let (system_prompt, tools, mut event, built) = prompt::build(
            &self.prompt,
            &self.model.reference,
            unattended,
            &self.tools,
            self.provider.as_ref(),
            self.preamble_reason,
            self.replaced.clone(),
            self.handoff.trigger_at,
            self.tool_search.as_ref(),
        );
        // The loop owns the effective budget; the builder leaves it
        // absent and every build passes through here.
        event.budget = self.budget;
        #[cfg(test)]
        if crate::timing_tests::take_fail_preamble_write() {
            return Err(crate::timing_tests::log_io_error());
        }
        let written = Event::PreambleBuilt(event.clone());
        self.log.append(&written, None, None)?;
        self.summary.observe(&written, None);
        let large = prompt::definitions_notice(&event);
        // The effective deferred set the requests render logged searches
        // against (`docs/tools.md`, "Tool search"): `None` is a model
        // without deferral, `Some(D)` the deferred names, empty or not.
        let deferred: Option<BTreeSet<String>> =
            (self.tool_search.is_some() && self.provider.defers()).then(|| {
                event
                    .tools
                    .iter()
                    .filter(|tool| tool.deferred)
                    .map(|tool| tool.name.clone())
                    .collect()
            });
        let mut search = None;
        if let Some((tool, definition)) = built {
            let name = definition.name.clone();
            self.tools
                .insert(name.clone(), ("builtin".to_owned(), tool, definition));
            search = Some(name);
        }
        // The insert rides the loop until the next build drops it.
        self.inserted_search = search;
        self.preamble = Some(Preamble {
            system_prompt,
            tools,
            // A fresh build sends what its own tools wire.
            sent_tools: None,
            tool_choice: event.tool_choice,
            cache_lifetime: event.cache_lifetime,
            thinking: self.prompt.thinking,
            deferred,
        });
        self.publish_preamble_rows(dropped.as_deref());
        // The scope ends with the installation, before the opening: the
        // opening and its notices are session content, not the build.
        drop(_timer);
        #[cfg(test)]
        crate::timing_tests::preamble_exit(self.log.clock().as_ref());
        Ok(large)
    }

    /// Writes the opening message and its notices, once per session: after
    /// `preamble_built` and before `turn_started`
    /// (`docs/system-prompt.md`, "The opening message"). A resume over a
    /// log that already holds one writes none. The message goes through
    /// the same path as every durable event, so it renders into the
    /// conversation as its first `User` message, before the turn's input.
    fn ensure_opening(&mut self) -> Result<bool, Error> {
        if self.opened {
            return Ok(false);
        }
        self.opened = true;
        self.write_opening(None)?;
        Ok(true)
    }

    /// The turn-start instruction-file, skill and date check
    /// (`docs/system-prompt.md`, "When something changes", "Added and
    /// removed skills" and "The date"), before `turn_started`: one
    /// `instruction_file` per change in path order, then the skill
    /// notices, then `skills_changed`, then `date_changed`.
    fn check_changes(&mut self) -> Result<(), Error> {
        let out = self.changes.check(self.prompt.clock.as_ref());
        for event in out
            .files
            .into_iter()
            .map(Event::InstructionFile)
            .chain(out.notices.into_iter().map(Event::Notice))
        {
            self.append_early(&event)?;
        }
        let skills = self.skills.check();
        for notice in skills.notices {
            self.append_early(&Event::Notice(notice))?;
        }
        if let Some(changed) = skills.changed {
            self.append_early(&Event::SkillsChanged(changed))?;
        }
        if skills.commands_changed {
            for notice in self.refresh_commands(&skills.inputs, &skills.found) {
                self.append_early(&Event::Notice(notice))?;
            }
        }
        if let Some(date) = out.date {
            self.append_early(&Event::DateChanged(date))?;
        }
        Ok(())
    }

    /// Recomputes the session's `commands` rows from the check's inputs
    /// and exact discovery snapshot, plus the current prompt and extension
    /// rows, and replaces the session's stored answer with the
    /// complete result (`docs/invocation.md`, "What each command does").
    /// A change the check found never restores a stale or shadowed row:
    /// every row comes from the current sources, through the same
    /// precedence the construction sites build with. Without the wired
    /// refresh nothing moves.
    pub(crate) fn refresh_commands(
        &mut self,
        inputs: &prompt::PromptInputs,
        found: &[crate::skills::Found],
    ) -> Vec<contract::events::Notice> {
        let Some(refresh) = self.commands_refresh.as_ref() else {
            return Vec::new();
        };
        let prompts = self
            .server_prompts
            .as_ref()
            .map(|prompts| prompts.rows.as_slice())
            .unwrap_or(&[]);
        let commands =
            crate::commands::commands_from_found(inputs, found, prompts, &(refresh.extensions)());
        self.skills.sync_commands(&commands.rows);
        (refresh.replace)(commands.rows);
        commands.notices
    }

    /// Writes `event` before its turn started, when there is no turn id yet.
    fn append_early(&mut self, event: &Event) -> Result<(), Error> {
        self.write(event, None, None)
    }
}

impl Loop {
    /// Every tool a `ToolSearchResult` in the conversation holds loaded.
    /// The conversation keeps every search result since the last handoff,
    /// so this is the loaded set (`docs/tools.md`, "Tool search").
    pub(crate) fn loaded_names(&self) -> BTreeSet<String> {
        self.conversation
            .iter()
            .filter_map(|input| match input {
                Input::ToolSearchResult { loaded, .. } => Some(loaded.iter().cloned()),
                Input::User { .. }
                | Input::Assistant { .. }
                | Input::Reasoning { .. }
                | Input::ToolCall { .. }
                | Input::ToolResult { .. } => None,
            })
            .flatten()
            .collect()
    }

    /// Publishes every declared tool's row through `ToolRows`, when set:
    /// each tool's effective state, deferred or in full (`docs/tools.md`,
    /// "Seeing the tools"). A dropped search the rebuild did not declare
    /// again publishes its removal first, so `/tools` lists every declared
    /// tool and nothing else. With no setter the publication is skipped.
    fn publish_preamble_rows(&self, dropped: Option<&str>) {
        let Some(rows) = self.tool_rows.as_ref() else {
            return;
        };
        if let Some(name) = dropped.filter(|name| !self.tools.contains_key(*name)) {
            (rows.declare)(name, None);
        }
        let deferred = self
            .preamble
            .as_ref()
            .and_then(|preamble| preamble.deferred.as_ref());
        for (name, (by, _, definition)) in &self.tools {
            let state = if deferred.is_some_and(|deferred| deferred.contains(name)) {
                ToolState::Deferred
            } else {
                ToolState::Full
            };
            let source = match by.as_str() {
                "builtin" => ToolSource::Builtin,
                _ => ToolSource::Extension {
                    extension: by.clone(),
                },
            };
            let bytes = serde_json::to_vec(definition)
                .map(|encoded| encoded.len() as u64)
                .unwrap_or(u64::MAX);
            (rows.declare)(
                name,
                Some(ToolInfo {
                    name: name.clone(),
                    source,
                    state,
                    bytes,
                    tokens: None,
                }),
            );
            (rows.state)(name, state);
        }
    }

    /// Publishes `Loaded` for every name a completed search loaded that the
    /// request defers (`docs/tools.md`, "Tool search"): publishing a name
    /// that already shows `Loaded` is a no-op. With no deferral or no
    /// setter nothing publishes.
    pub(crate) fn publish_completion_rows(&self, completed: &ToolCallCompleted) {
        let Some(rows) = self.tool_rows.as_ref() else {
            return;
        };
        let Some(deferred) = self
            .preamble
            .as_ref()
            .and_then(|preamble| preamble.deferred.as_ref())
        else {
            return;
        };
        let Some(loaded) = completed.loaded.as_ref() else {
            return;
        };
        for name in loaded {
            if deferred.contains(name) {
                (rows.state)(name, ToolState::Loaded);
            }
        }
    }

    /// Moves the loaded names a completed handoff dropped back to
    /// `Deferred`: only names the loaded set lost publish (`docs/tools.md`,
    /// "Tool search"). With no deferral or no setter nothing publishes.
    pub(crate) fn publish_handoff_rows(&self, before: &BTreeSet<String>) {
        let Some(rows) = self.tool_rows.as_ref() else {
            return;
        };
        if self
            .preamble
            .as_ref()
            .and_then(|preamble| preamble.deferred.as_ref())
            .is_none()
        {
            return;
        }
        let after = self.loaded_names();
        for name in before.difference(&after) {
            (rows.state)(name, ToolState::Deferred);
        }
    }
}
