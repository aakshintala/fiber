//! Starting a turn (`docs/loop.md`, "Starting a turn"): the session's run
//! of turns, the one preamble build with the opening message, and the
//! instruction-file and date check, each written before `turn_started`.

use std::sync::Arc;

use contract::TurnId;
use contract::events::{Event, TurnOutcome, TurnStarted};

use crate::{Error, Loop, Preamble, cancel, inbox, mint, prompt, status};

impl Loop {
    /// Runs turns until `close` is taken or every sender of the inbox is
    /// gone, then, while jobs still run, the ending notice and their ends
    /// (`docs/invocation.md`, "Lifecycle"; `docs/tools.md`, "Background jobs").
    pub fn run(mut self) -> Result<(), Error> {
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
        result
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
        let (input, contexts, state) = self.turn_input(started.pieces);
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
            for (added, state) in &contexts {
                self.write_carried(state.clone(), Some(&turn))?;
                self.append(&Event::ContextAdded(added.clone()), &turn, None)?;
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
        if self.preamble.is_some() {
            return Ok(false);
        }
        // The post-offer load runs once per process, after the offer step
        // and before the first build: the turn path and `finish_suspended`
        // both run `offer` before this. A rewound loop's logged preamble
        // returned above, so no rebuild in that process runs the load.
        self.load_approved()?;
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
        let (system_prompt, tools, mut event) = prompt::build(
            &self.prompt,
            &self.model.reference,
            unattended,
            &self.tools,
            self.provider.as_ref(),
            self.preamble_reason,
            self.replaced.clone(),
            self.handoff.trigger_at,
        );
        // The loop owns the effective budget; the builder leaves it
        // absent and every build passes through here.
        event.budget = self.budget;
        #[cfg(test)]
        if crate::timing_tests::take_fail_preamble_write() {
            return Err(crate::timing_tests::log_io_error());
        }
        self.log
            .append(&Event::PreambleBuilt(event.clone()), None, None)?;
        let large = prompt::definitions_notice(&event);
        self.preamble = Some(Preamble {
            system_prompt,
            tools,
            // A fresh build sends what its own tools wire.
            sent_tools: None,
            tool_choice: event.tool_choice,
            cache_lifetime: event.cache_lifetime,
            thinking: self.prompt.thinking,
        });
        // The scope ends with the installation, before the opening: the
        // opening and its notices are session content, not the build.
        drop(_timer);
        #[cfg(test)]
        crate::timing_tests::preamble_exit(self.log.clock().as_ref());
        let opened = self.ensure_opening()?;
        if let Some(notice) = large {
            self.append_early(&Event::Notice(notice))?;
        }
        Ok(opened)
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
        if let Some(date) = out.date {
            self.append_early(&Event::DateChanged(date))?;
        }
        Ok(())
    }

    /// Writes `event` before its turn started, when there is no turn id yet.
    fn append_early(&mut self, event: &Event) -> Result<(), Error> {
        self.write(event, None, None)
    }
}
