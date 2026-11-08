//! Keeper parentage and crash recovery through the built binary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "binary test assertions"
)]

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::Watchdog;
use serde_json::{Value, json};
use support::{
    Deadline, KillGroup, Setup, Socket, Subscribed, complete_lines, recv_answer, run_to_exit,
    session_dir, spawn_watched, subscribe_direct, until_after, write_json,
};

fn script(setup: &Setup, name: &str, steps: Value) {
    write_json(&setup.workspace().join(name), &json!({"steps":steps}));
}
fn configured() -> Setup {
    let setup = Setup::new();
    script(&setup, "s.json", json!([{"text":"Ready."}]));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"scripted/s.json", "sandbox":{"enabled":false}}),
    );
    setup
}
fn probe(
    connect: impl FnOnce() -> std::io::Result<UnixStream>,
    last: &Mutex<Option<String>>,
) -> bool {
    match connect() {
        Ok(_) => true,
        Err(error) => {
            *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
            false
        }
    }
}

fn timeout_diagnostic(last_error: Option<String>, path: &PathBuf) -> String {
    let connect_error = last_error.unwrap_or_else(|| "no connect error was observed".to_owned());
    let socket_state = match fs::metadata(path) {
        Ok(metadata) => format!("present (file type: {:?})", metadata.file_type()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent".to_owned(),
        Err(error) => format!("metadata error: {error}"),
    };
    format!(
        "last connect error: {connect_error}; socket path {}: {socket_state}",
        path.display()
    )
}

fn wait_for_refuse_only_socket(deadline: Deadline, path: PathBuf) {
    let last_error = Arc::new(Mutex::new(None));
    let error_slot = Arc::clone(&last_error);
    let target = path.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fakes::wait_until("keeper refuse-only socket to accept", deadline, move || {
            probe(|| UnixStream::connect(&target), &error_slot)
        });
    }));
    if result.is_err() {
        let diagnostic = timeout_diagnostic(
            last_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            &path,
        );
        panic!("waited until the deadline for keeper refuse-only socket to accept; {diagnostic}");
    }
}

fn run(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    run_to_exit(
        setup.deadline.start_wait(),
        "fiber keeper acceptance invocation",
        command,
    )
}
fn stdout(child: &mut Child) -> mpsc::Receiver<String> {
    let out = child.stdout.take().unwrap();
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    lines
}
fn event(lines: &mpsc::Receiver<String>, kind: &str, deadline: Deadline) -> Value {
    let wait = deadline.start_wait();
    loop {
        let text = wait
            .recv(lines)
            .unwrap_or_else(|error| panic!("waiting for live stdout {kind}: {error}"));
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == kind {
            return line;
        }
    }
}
fn reap(mut child: Child, deadline: Deadline) -> std::process::ExitStatus {
    let (send, result) = mpsc::channel();
    thread::spawn(move || match send.send(child.wait()) {
        Ok(()) | Err(_) => {}
    });
    deadline.start_wait().recv(&result).unwrap().unwrap()
}

/// A crashed session whose keeper holds one background job: the seeded id,
/// both fifos, the `echo $$` / `cat` command and the two-call script the
/// keeper-bound tests share.
struct BoundFixture {
    setup: Setup,
    id: String,
    keeper_pattern: String,
    keeper_watchdog: Watchdog,
    marker: String,
    job_watchdog: Watchdog,
    ready: fakes::children::Ready,
    gate: PathBuf,
}

fn bound_fixture() -> BoundFixture {
    fixture_for(|marker, ready, gate| {
        format!(
            "sh -c 'echo before-{marker}; echo $$ > {}; cat {}; echo done-{marker}' {marker}",
            ready.path().display(),
            gate.display()
        )
    })
}

/// The fixture with the background command `job` builds from the marker,
/// the ready fifo the job reports its group on, and the gate it waits on.
fn fixture_for(
    job: impl FnOnce(&str, &fakes::children::Ready, &std::path::Path) -> String,
) -> BoundFixture {
    let setup = configured();
    // Seed the id before the command so its keeper watchdog is armed first.
    let seed = run(&setup, &["ask", "seed"]);
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let seed = complete_lines(&String::from_utf8(seed.stdout).unwrap());
    let id = seed[0]["session_id"].as_str().unwrap().to_owned();
    let keeper_pattern = format!("keeper --id {id}");
    let keeper_watchdog = Watchdog::matching(&keeper_pattern);
    let marker = format!(
        "keeper-job-{}",
        setup.root.path().file_name().unwrap().to_str().unwrap()
    );
    let job_watchdog = Watchdog::matching(&marker);
    let ready = fakes::children::Ready::new(&setup.workspace());
    let gate = setup.workspace().join("release.fifo");
    let hold = setup.workspace().join("hold-session.fifo");
    fakes::fifo(&gate);
    fakes::fifo(&hold);
    let command = job(&marker, &ready, &gate);
    // The second foreground call holds the session open without a timing
    // sleep. Its start also fences the first call's completed adoption.
    let foreground = format!("sh -c 'cat {}' {marker}-tool", hold.display());
    script(
        &setup,
        "job.json",
        json!([
            {"tool_calls":[{"name":"shell","arguments":{"command":command,"run_in_background":true}}]},
            {"tool_calls":[{"name":"shell","arguments":{"command":foreground}}]}
        ]),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision":"allow","tool":"shell","prefix":command}),
            json!({"decision":"allow","tool":"shell","prefix":foreground})
        ),
    )
    .unwrap();
    BoundFixture {
        setup,
        id,
        keeper_pattern,
        keeper_watchdog,
        marker,
        job_watchdog,
        ready,
        gate,
    }
}

/// What a crashed session leaves: its one background job under the keeper.
struct Crashed {
    /// The job's durable id.
    job: String,
    /// The job's output file.
    output: PathBuf,
    /// The job's process group, to reap what a test leaves behind.
    job_group: u32,
}

/// Runs the fixture's session until its job is held by the keeper and kills
/// the session's whole process group. `bound_ms` sets the keeper's bound for
/// the run. The keeper's socket accepts when this returns.
fn crash(fixture: &BoundFixture, bound_ms: Option<&str>) -> Crashed {
    crash_after(fixture, bound_ms, |_| {})
}

/// [`crash`], after `settle` has seen the job's output file with the job
/// reported ready: bytes a live session was sent and had not read die with it.
fn crash_after(
    fixture: &BoundFixture,
    bound_ms: Option<&str>,
    settle: impl FnOnce(&std::path::Path),
) -> Crashed {
    let setup = &fixture.setup;
    let mut ask = setup.fiber(&[
        "ask",
        "--resume",
        &fixture.id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    ask.current_dir(setup.workspace());
    if let Some(bound_ms) = bound_ms {
        ask.env("FIBER_KEEPER_BOUND_MS", bound_ms);
    }
    let (mut child, session_watchdog) = spawn_watched(&mut ask);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    let started = event(&lines, "job_started", setup.deadline);
    event(&lines, "tool_call_started", setup.deadline);
    let job_group = fixture.ready.wait(setup.deadline.start_wait())[0];
    let output =
        session_dir(setup, &fixture.id).join(started["payload"]["output_path"].as_str().unwrap());
    settle(&output);
    fakes::kill_group(group, "KILL").unwrap();
    assert!(!reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());
    // The bind comes after the killed session released its socket.
    // Do not subscribe: the keeper accepts and closes without protocol.
    let socket = setup.session_socket(&fixture.id);
    wait_for_refuse_only_socket(setup.deadline.start_wait(), socket);
    Crashed {
        job: started["payload"]["job_id"].as_str().unwrap().to_owned(),
        output: session_dir(setup, &fixture.id)
            .join(started["payload"]["output_path"].as_str().unwrap()),
        job_group,
    }
}

/// A resumed session serving a socket, killed with its group on drop.
struct Serving {
    child: Option<Child>,
    group: u32,
    watchdog: Option<Watchdog>,
    socket: PathBuf,
    /// The subscribed connection, once attached.
    link: Option<(Socket, Subscribed)>,
    deadline: Deadline,
    guard: Option<KillGroup>,
}

/// `fiber session --resume` on the fixture's crashed session, subscribed.
fn serve_resume(setup: &Setup, id: &str, extra: &[&str]) -> Serving {
    let mut serving = serve_resume_unattached(setup, id, extra);
    serving.attach();
    serving
}

/// [`serve_resume`] with no client connected: nothing subscribes to the
/// session's stream until [`Serving::attach`].
fn serve_resume_unattached(setup: &Setup, id: &str, extra: &[&str]) -> Serving {
    let workspace = setup.workspace();
    let mut args = vec![
        "session",
        "--id",
        id,
        "--workspace",
        workspace.to_str().unwrap(),
        "--resume",
    ];
    args.extend(extra);
    let mut command = setup.fiber(&args);
    command.current_dir(&workspace);
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    event(&lines, "fiber_started", setup.deadline);
    Serving {
        child: Some(child),
        group,
        watchdog: Some(watchdog),
        socket: setup.session_socket(id),
        link: None,
        deadline: setup.deadline,
        guard: Some(guard),
    }
}

impl Serving {
    fn pid(&self) -> u32 {
        self.group
    }

    /// Connects and subscribes at the full level.
    fn attach(&mut self) {
        let client = Socket::connect(self.deadline, &self.socket);
        let sub = subscribe_direct(&client);
        self.link = Some((client, sub));
    }

    fn client(&self) -> &Socket {
        &self.link.as_ref().expect("attached").0
    }

    /// Lines of the stream until `done`.
    #[track_caller]
    fn until(&self, what: &str, done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let (client, sub) = self.link.as_ref().expect("attached");
        until_after(client, sub, what, done)
    }

    fn send(&self, line: &str) {
        self.client().send(line);
    }

    /// Kills the session's whole process group, as a crash does.
    fn crash(&mut self) {
        fakes::kill_group(self.group, "KILL").unwrap();
        assert!(!reap(self.child.take().unwrap(), self.deadline).success());
        assert!(fakes::group_empties(self.group, self.deadline.start_wait()));
        std::mem::forget(self.guard.take());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(self.deadline.cleanup_phase());
    }

    /// Ends the session with `close` and reaps it.
    fn close(mut self) {
        if self.link.is_none() {
            self.attach();
        }
        self.client().send(r#"{"id":"c_close","command":"close"}"#);
        assert_eq!(
            recv_answer(self.client(), "c_close", "close accepted")["kind"],
            "command_accepted"
        );
        assert!(reap(self.child.take().unwrap(), self.deadline).success());
        assert!(fakes::group_empties(self.group, self.deadline.start_wait()));
        std::mem::forget(self.guard.take());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(self.deadline.cleanup_phase());
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            drop(self.guard.take());
            watchdog.stand_down(self.deadline.cleanup_phase());
        }
    }
}

fn release(path: &std::path::Path, deadline: Deadline) {
    let path = path.to_path_buf();
    support::bounded(deadline.start_wait(), "releasing a held job", move || {
        fs::write(path, b"").unwrap();
    });
}

fn log_lines(setup: &Setup, id: &str) -> Vec<Value> {
    complete_lines(&fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap())
}

fn completions(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .collect()
}

#[test]
fn a_crash_leaves_the_job_to_the_keeper_and_the_resume_delivers_it() {
    let fixture = bound_fixture();
    let crashed = crash(&fixture, None);
    let setup = &fixture.setup;
    let id = fixture.id.as_str();
    // The job ends while no session is there to take the log.
    release(&fixture.gate, setup.deadline);
    assert!(fakes::matching_exits(
        &fixture.keeper_pattern,
        setup.deadline.start_wait()
    ));
    script(setup, "done.json", json!([{"text":"Recovered."}]));
    let resumed = run(
        setup,
        &["ask", "--resume", id, "--model", "scripted/done.json", "go"],
    );
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let lines = log_lines(setup, id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 1);
    let end = &ends[0]["payload"];
    assert_eq!(end["job_id"], crashed.job);
    assert_eq!(end["status"], "completed");
    assert_eq!(end["process"]["exit_code"], 0);
    for field in ["output_tail", "notice", "changed_by"] {
        assert!(
            end.get(field).is_none(),
            "keeper must not set {field}: {end}"
        );
    }
    assert!(
        !lines.iter().any(|line| line["kind"] == "job_completed"
            && line["payload"]["error"]["code"] == "orphaned")
    );
    let ended_at = lines
        .iter()
        .position(|line| line["kind"] == "job_completed")
        .unwrap();
    let resumed_at = lines
        .iter()
        .rposition(|line| line["kind"] == "fiber_started")
        .unwrap();
    assert!(
        ended_at < resumed_at,
        "docs/tools.md, \"The keeper\": keeper appends the job's end while the session is gone"
    );
    assert!(
        fs::read_to_string(&crashed.output)
            .unwrap()
            .contains(&format!("done-{}", fixture.marker))
    );
    let turn = lines
        .iter()
        .skip(resumed_at)
        .find(|line| line["kind"] == "turn_started")
        .unwrap();
    let notices: Vec<_> = turn["payload"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["source"] == "fiber")
        .collect();
    assert_eq!(notices.len(), 1, "{turn}");
    let text = notices[0].to_string();
    assert!(text.contains(&crashed.job) && text.contains("completed"));
    assert!(fakes::matching_exits(
        &fixture.marker,
        setup.deadline.start_wait()
    ));
    fixture
        .keeper_watchdog
        .stand_down(setup.deadline.cleanup_phase());
    fixture
        .job_watchdog
        .stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_resume_takes_a_running_job_from_its_keeper_and_writes_its_ending() {
    let fixture = bound_fixture();
    let crashed = crash(&fixture, None);
    let setup = &fixture.setup;
    let id = fixture.id.as_str();
    let dir = session_dir(setup, id);
    let prefix = format!("before-{}\n", fixture.marker);
    // The keeper has written the job's first bytes: what the resume must keep.
    let output = crashed.output.clone();
    let want = prefix.clone();
    fakes::wait_until(
        "the keeper to write the job's first line",
        setup.deadline.start_wait(),
        move || fs::read_to_string(&output).is_ok_and(|text| text == want),
    );
    let lines_before = log_lines(setup, id).len();
    let serving = serve_resume(setup, id, &["--model", "scripted/s.json"]);
    // The lock is the resumed process's, not the keeper's: from here only the
    // session can write the job's ending, whenever the job ends.
    let pid = serving.pid().to_string();
    let holder = dir.clone();
    fakes::wait_until(
        "the resumed process to hold the lock",
        setup.deadline.start_wait(),
        move || log::lock_label(&holder).as_deref() == Some(pid.as_str()),
    );
    release(&fixture.gate, setup.deadline);
    serving.until("the restored job's ending", |line| {
        line["kind"] == "job_completed"
    });
    // The keeper leaves once the session holds the ending durably.
    assert!(fakes::matching_exits(
        &fixture.keeper_pattern,
        setup.deadline.start_wait()
    ));
    let lines = log_lines(setup, id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 1, "one ending: {ends:?}");
    let end = &ends[0]["payload"];
    assert_eq!(end["job_id"], crashed.job);
    assert_eq!(end["status"], "completed");
    assert_eq!(end["process"]["exit_code"], 0);
    // Only the session fills the output tail; the keeper never does.
    assert!(
        end["output_tail"]
            .as_str()
            .is_some_and(|tail| tail.contains(&format!("done-{}", fixture.marker))),
        "{end}"
    );
    let resumed_at = lines
        .iter()
        .rposition(|line| line["kind"] == "fiber_started")
        .unwrap();
    assert!(resumed_at >= lines_before);
    let ended_at = lines
        .iter()
        .position(|line| line["kind"] == "job_completed")
        .unwrap();
    assert!(ended_at > resumed_at, "the resumed process wrote it");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "job_started"
                && line["payload"]["job_id"] == crashed.job)
            .count(),
        1,
        "no second durable start"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line["payload"]["error"]["code"] == "orphaned"),
        "nothing is orphaned"
    );
    // The keeper's bytes stay and the session's follow, once.
    assert_eq!(
        fs::read_to_string(&crashed.output).unwrap(),
        format!("{prefix}done-{}\n", fixture.marker)
    );
    serving.close();
    assert!(fakes::matching_exits(
        &fixture.marker,
        setup.deadline.start_wait()
    ));
    fixture
        .keeper_watchdog
        .stand_down(setup.deadline.cleanup_phase());
    fixture
        .job_watchdog
        .stand_down(setup.deadline.cleanup_phase());
}

/// Whether process `pid` still exists.
fn alive(pid: u32) -> bool {
    fakes::pid_alive(pid)
}

fn parent_of(pid: u32) -> u32 {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// The `job_stop` command line for `job`, answered as `c_stop_<job>`.
fn stop_line(job: &str) -> String {
    format!(r#"{{"id":"c_stop_{job}","command":"job_stop","args":{{"job_id":"{job}"}}}}"#)
}

/// A resumed session holding the old job, and a second job it started after
/// the handover, which runs under a keeper of its own.
struct TwoKeepers {
    fixture: BoundFixture,
    crashed: Crashed,
    serving: Serving,
    old_keeper: u32,
    new_keeper: u32,
    second: String,
    second_group: u32,
    second_gate: PathBuf,
    /// Reaps both jobs' groups if the test ends early.
    _guards: [KillGroup; 2],
}

/// Crashes the fixture's session, resumes it, and prompts the model to start
/// a second background command. The script then answers each later wake, such
/// as a completion notice, with a line of text.
fn two_keepers() -> TwoKeepers {
    let fixture = bound_fixture();
    let crashed = crash(&fixture, None);
    let setup = &fixture.setup;
    let old_keeper = fakes::matching(&fixture.keeper_pattern, setup.deadline.left())
        .unwrap()
        .into_iter()
        .max()
        .expect("the old keeper runs");
    let second_marker = format!("{}-two", fixture.marker);
    let second_dir = setup.workspace().join("second");
    fs::create_dir_all(&second_dir).unwrap();
    let ready = fakes::children::Ready::new(&second_dir);
    let second_gate = second_dir.join("release.fifo");
    fakes::fifo(&second_gate);
    let command = format!(
        "sh -c 'echo $$ > {}; cat {}; echo done-{second_marker}' {second_marker}",
        ready.path().display(),
        second_gate.display()
    );
    script(
        setup,
        "two.json",
        json!([
            {"tool_calls":[{"name":"shell","arguments":{"command":command,"run_in_background":true}}]},
            {"text":"Started."},
            {"text":"Noted."},
            {"text":"Noted."},
            {"text":"Noted."},
            {"text":"Noted."}
        ]),
    );
    let mut rules = fs::read_to_string(setup.home().join("rules")).unwrap();
    rules.push_str(&format!(
        "{}\n",
        json!({"decision":"allow","tool":"shell","prefix":command})
    ));
    fs::write(setup.home().join("rules"), rules).unwrap();
    let serving = serve_resume(setup, &fixture.id, &["--model", "scripted/two.json"]);
    serving.send(
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"start another"}]}}"#,
    );
    let started = serving.until("the second job to start", |line| {
        line["kind"] == "job_started" && line["payload"]["job_id"] != crashed.job.as_str()
    });
    let second = started.last().unwrap()["payload"]["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_group = ready.wait(setup.deadline.start_wait())[0];
    let new_keeper = fakes::matching(&fixture.keeper_pattern, setup.deadline.left())
        .unwrap()
        .into_iter()
        .find(|pid| *pid != old_keeper)
        .expect("a fresh keeper runs");
    let guards = [KillGroup(crashed.job_group), KillGroup(second_group)];
    TwoKeepers {
        fixture,
        crashed,
        serving,
        old_keeper,
        new_keeper,
        second,
        second_group,
        second_gate,
        _guards: guards,
    }
}

impl TwoKeepers {
    /// Ends both jobs and the keepers: the test-owned cleanup.
    fn reap(self) {
        let deadline = self.fixture.setup.deadline;
        if self.serving.child.is_some() {
            self.serving.close();
        }
        for group in [self.crashed.job_group, self.second_group] {
            drop(fakes::kill_group(group, "KILL"));
            assert!(fakes::group_empties(group, deadline.start_wait()));
        }
        std::mem::forget(self._guards);
        self.fixture
            .keeper_watchdog
            .stand_down(deadline.cleanup_phase());
        self.fixture
            .job_watchdog
            .stand_down(deadline.cleanup_phase());
    }
}

#[test]
fn a_new_command_after_a_handover_runs_under_a_fresh_keeper_and_each_stop_reaches_its_own() {
    let two = two_keepers();
    let setup = &two.fixture.setup;
    // The fresh keeper is the resumed process's child; the old job stays
    // under the keeper that was already running it.
    assert_eq!(parent_of(two.new_keeper), two.serving.pid());
    assert_ne!(parent_of(two.old_keeper), two.serving.pid());
    assert!(alive(two.old_keeper));
    // Both keepers named their command `call_0`: each stop must reach the
    // job it names, on its own keeper.
    two.serving.send(&stop_line(&two.crashed.job));
    two.serving.until("the old job's ending", |line| {
        line["kind"] == "job_completed" && line["payload"]["job_id"] == two.crashed.job.as_str()
    });
    assert!(fakes::group_empties(
        two.crashed.job_group,
        setup.deadline.start_wait()
    ));
    // The second job did not feel it.
    assert!(fakes::group_alive(two.second_group));
    assert!(alive(two.new_keeper));
    // The old keeper leaves once its last job's ending is durable.
    let old_keeper = two.old_keeper;
    fakes::wait_until(
        "the old keeper to exit",
        setup.deadline.start_wait(),
        move || !alive(old_keeper),
    );
    let lines = log_lines(setup, &two.fixture.id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["payload"]["job_id"], two.crashed.job);
    two.serving.send(&stop_line(&two.second));
    two.serving.until("the second job's ending", |line| {
        line["kind"] == "job_completed" && line["payload"]["job_id"] == two.second.as_str()
    });
    assert!(fakes::group_empties(
        two.second_group,
        setup.deadline.start_wait()
    ));
    let lines = log_lines(setup, &two.fixture.id);
    assert_eq!(completions(&lines).len(), 2);
    assert!(
        !lines
            .iter()
            .any(|line| line["payload"]["error"]["code"] == "orphaned")
    );
    two.reap();
}

#[test]
fn a_second_crash_leaves_one_keeper_the_log_and_the_third_resume_orphans_the_other() {
    let mut two = two_keepers();
    let setup = &two.fixture.setup;
    let id = two.fixture.id.clone();
    let dir = session_dir(setup, &id);
    two.serving.crash();
    let crashed_log = fs::read(dir.join("events.jsonl")).unwrap();
    // Two keepers race for the lock and either may win. The label names the
    // winner once it holds the lock, and the socket accepts once it serves.
    let holder = dir.clone();
    fakes::wait_until(
        "a keeper to take the lock",
        setup.deadline.start_wait(),
        move || log::lock_label(&holder).is_some_and(|label| label.starts_with("keeper ")),
    );
    let winner: u32 = log::lock_label(&dir)
        .unwrap()
        .strip_prefix("keeper ")
        .unwrap()
        .parse()
        .unwrap();
    assert!([two.old_keeper, two.new_keeper].contains(&winner));
    let socket = setup.session_socket(&id);
    fakes::wait_until(
        "the winning keeper's socket to accept",
        setup.deadline.start_wait(),
        move || std::os::unix::net::UnixStream::connect(&socket).is_ok(),
    );
    let (loser, won, lost) = if winner == two.old_keeper {
        (two.new_keeper, &two.crashed.job, &two.second)
    } else {
        (two.old_keeper, &two.second, &two.crashed.job)
    };
    let (won_group, lost_group, won_gate) = if winner == two.old_keeper {
        (
            two.crashed.job_group,
            two.second_group,
            two.fixture.gate.clone(),
        )
    } else {
        (
            two.second_group,
            two.crashed.job_group,
            two.second_gate.clone(),
        )
    };
    // The loser stops tool calls only, writes nothing and leaves.
    fakes::wait_until("the losing keeper to exit", setup.deadline.start_wait(), {
        move || !alive(loser)
    });
    assert!(alive(winner));
    assert_eq!(
        fs::read(dir.join("events.jsonl")).unwrap(),
        crashed_log,
        "neither keeper appended: no job ended"
    );
    // Its job runs on, with no keeper left to account for it.
    assert!(fakes::group_alive(lost_group));
    assert!(fakes::group_alive(won_group));

    // The third resume takes the winner's job and orphans the loser's.
    // A script of text only: the notices wake the model, which starts nothing.
    script(
        setup,
        "quiet.json",
        json!([{"text":"Noted."},{"text":"Noted."},{"text":"Noted."},{"text":"Noted."}]),
    );
    let third = serve_resume(setup, &id, &["--model", "scripted/quiet.json"]);
    let holder = dir.clone();
    let pid = third.pid().to_string();
    fakes::wait_until(
        "the third process to hold the lock",
        setup.deadline.start_wait(),
        move || log::lock_label(&holder).as_deref() == Some(pid.as_str()),
    );
    let settled = dir.clone();
    fakes::wait_until(
        "the loser's job to be settled",
        setup.deadline.start_wait(),
        move || {
            fs::read_to_string(settled.join("events.jsonl"))
                .is_ok_and(|text| text.contains(r#""code":"orphaned""#))
        },
    );
    let lines = log_lines(setup, &id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 1, "only the loser's job is settled: {ends:?}");
    assert_eq!(ends[0]["payload"]["job_id"], lost.as_str());
    assert_eq!(ends[0]["payload"]["error"]["code"], "orphaned");
    // The winner's job is the session's again, running.
    assert!(fakes::group_alive(won_group));
    release(&won_gate, setup.deadline);
    third.until("the winner's job to end", |line| {
        line["kind"] == "job_completed" && line["payload"]["job_id"] == won.as_str()
    });
    fakes::wait_until(
        "the winning keeper to exit",
        setup.deadline.start_wait(),
        move || !alive(winner),
    );
    let lines = log_lines(setup, &id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 2, "{ends:?}");
    let ended: Vec<_> = ends
        .iter()
        .map(|end| {
            (
                end["payload"]["job_id"].as_str().unwrap(),
                &end["payload"]["status"],
            )
        })
        .collect();
    assert!(
        ended.contains(&(lost.as_str(), &json!("failed"))),
        "{ended:?}"
    );
    assert!(
        ended.contains(&(won.as_str(), &json!("completed"))),
        "{ended:?}"
    );
    third.close();
    two.reap();
}

const MIB: u64 = 1 << 20;
const CHUNK: usize = 1 << 20;
/// One chunk of the flood: a mebibyte of 64-byte lines that name their chunk
/// and their place in it.
fn flood_chunk(chunk: u64) -> Vec<u8> {
    let pad = "x".repeat(53);
    let mut bytes = Vec::with_capacity(CHUNK);
    for line in 0..16_384 {
        bytes.extend_from_slice(format!("{chunk:03}-{line:05}-{pad}\n").as_bytes());
    }
    bytes
}

/// Reads `chunks` of the file at `path`, one mebibyte at a time, and
/// compares each with the flood's: exact length, bytes and order.
fn assert_flood(path: &std::path::Path, chunks: std::ops::Range<u64>) {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = fs::File::open(path).unwrap();
    let mut got = vec![0; CHUNK];
    for chunk in chunks {
        file.seek(SeekFrom::Start(chunk * MIB)).unwrap();
        file.read_exact(&mut got).unwrap();
        assert!(got == flood_chunk(chunk), "chunk {chunk} differs");
    }
}

fn len_of(path: &std::path::Path) -> u64 {
    fs::metadata(path).map_or(0, |meta| meta.len())
}

/// Whether process `pid` holds `path` open, by its descriptors.
#[cfg(target_os = "linux")]
fn holds_open(pid: u32, path: &std::path::Path) -> bool {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| fs::read_link(entry.path()).is_ok_and(|target| target == path))
}

/// The peak resident set of process `pid`, in KiB.
#[cfg(target_os = "linux")]
fn peak_rss_kib(pid: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn keeper_handover_flood_exact_output_and_rss() {
    // Three phases of 24 MiB each: before the crash, while only the keeper
    // is there, and after the resume holds the job.
    let mut second = None;
    let mut second_gate = None;
    let fixture = fixture_for(|marker, ready, gate| {
        let dir = ready.path().parent().unwrap().to_path_buf();
        let phase_two = dir.join("phase-two");
        fs::create_dir_all(&phase_two).unwrap();
        second = Some(fakes::children::Ready::new(&phase_two));
        let release = phase_two.join("release.fifo");
        fakes::fifo(&release);
        second_gate = Some(release.clone());
        let script = dir.join("flood.sh");
        fs::write(
            &script,
            format!(
                r#"emit() {{ awk -v c="$1" 'BEGIN{{pad=sprintf("%53s","");gsub(/ /,"x",pad);for(j=0;j<16384;j++)printf "%03d-%05d-%s\n",c,j,pad}}'; }}
i=0
while [ $i -lt 24 ]; do emit $i; i=$((i+1)); done
echo $$ > {first}
cat {gate}
while [ $i -lt 48 ]; do emit $i; i=$((i+1)); done
echo $$ > {next}
cat {release}
while [ $i -lt 72 ]; do emit $i; i=$((i+1)); done
"#,
                first = ready.path().display(),
                gate = gate.display(),
                next = second.as_ref().unwrap().path().display(),
                release = release.display(),
            ),
        )
        .unwrap();
        format!("sh {} {marker}", script.display())
    });
    let (second, second_gate) = (second.unwrap(), second_gate.unwrap());
    let setup = &fixture.setup;
    let id = fixture.id.as_str();
    // Phase one is all in the file before the crash: what a live session was
    // sent and had not read would die with it.
    let deadline = setup.deadline;
    let crashed = crash_after(&fixture, None, |output| {
        let watched = output.to_path_buf();
        fakes::wait_until("phase one in the file", deadline.start_wait(), move || {
            len_of(&watched) >= 24 * MIB
        });
    });
    let _guard = KillGroup(crashed.job_group);
    let output = crashed.output.clone();
    // Phase two: only the keeper writes it, into the file.
    release(&fixture.gate, setup.deadline);
    second.wait(setup.deadline.start_wait());
    let watched = output.clone();
    fakes::wait_until(
        "phase two in the file",
        setup.deadline.start_wait(),
        move || len_of(&watched) >= 48 * MIB,
    );
    assert_eq!(len_of(&output), 48 * MIB);
    assert_flood(&output, 0..48);
    let keeper = fakes::matching(&fixture.keeper_pattern, setup.deadline.left())
        .unwrap()
        .into_iter()
        .max()
        .unwrap();
    assert!(holds_open(keeper, &output), "the keeper writes the file");

    script(
        setup,
        "quiet.json",
        json!([{"text":"Noted."},{"text":"Noted."}]),
    );
    // Nobody subscribes during the flood: the peak measures the handover.
    let serving = serve_resume_unattached(setup, id, &["--model", "scripted/quiet.json"]);
    let pid = serving.pid();
    let dir = session_dir(setup, id);
    fakes::wait_until(
        "the resumed process to hold the lock",
        setup.deadline.start_wait(),
        move || log::lock_label(&dir).as_deref() == Some(pid.to_string().as_str()),
    );
    // The keeper has switched the job to frames once it closes the file.
    let watched = output.clone();
    fakes::wait_until(
        "the keeper to stop writing the file",
        setup.deadline.start_wait(),
        move || !holds_open(keeper, &watched),
    );
    assert!(holds_open(pid, &output), "the session holds the file now");
    // The peak so far: a debug build starts far above the release ceiling, so
    // the bound is on what the flood adds, as for a foreground flood.
    let before = peak_rss_kib(pid);
    // Phase three reaches the file only through the session.
    release(&second_gate, setup.deadline);
    let ended = session_dir(setup, id);
    fakes::wait_until(
        "the flood's ending",
        setup.deadline.start_wait(),
        move || {
            fs::read_to_string(ended.join("events.jsonl"))
                .is_ok_and(|text| text.contains(r#""kind":"job_completed""#))
        },
    );
    assert_eq!(len_of(&output), 72 * MIB, "all output precedes the ending");
    assert_flood(&output, 0..72);
    let lines = log_lines(setup, id);
    let ends = completions(&lines);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["payload"]["job_id"], crashed.job);
    assert_eq!(ends[0]["payload"]["status"], "completed");
    // 24 MiB went through the session as frames, and no queue held them.
    let peak = peak_rss_kib(pid);
    assert!(
        peak <= before + 4096,
        "peak RSS grew by more than 4 MiB: {before} KiB to {peak} KiB"
    );
    // docs/performance.md gates the resumed session's absolute peak in a
    // release build on Linux x86_64.
    if cfg!(all(target_arch = "x86_64", not(debug_assertions))) {
        assert!(peak <= 46_720, "peak RSS {peak} KiB");
    }
    serving.close();
    assert!(fakes::matching_exits(
        &fixture.keeper_pattern,
        setup.deadline.start_wait()
    ));
    assert!(fakes::matching_exits(
        &fixture.marker,
        setup.deadline.start_wait()
    ));
    std::mem::forget(_guard);
    fixture
        .keeper_watchdog
        .stand_down(setup.deadline.cleanup_phase());
    fixture
        .job_watchdog
        .stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn refuse_only_socket_timeout_reports_connect_error_and_path_state() {
    let setup = Setup::new();
    let path = setup.workspace().join("missing-socket");
    let last = Mutex::new(None);
    assert!(!probe(
        || Err(std::io::Error::other("probe failure")),
        &last,
    ));
    assert_eq!(last.lock().unwrap().as_deref(), Some("probe failure"));
    let diagnostic = timeout_diagnostic(last.into_inner().unwrap(), &path);
    assert!(diagnostic.contains("socket path "), "{diagnostic}");
    assert!(diagnostic.contains(": absent"), "{diagnostic}");
    assert!(
        diagnostic.contains("last connect error: probe failure"),
        "{diagnostic}"
    );
    let last = Mutex::new(None);
    let no_observation = timeout_diagnostic(last.into_inner().unwrap(), &path);
    assert!(
        no_observation.contains("last connect error: no connect error was observed"),
        "{no_observation}"
    );
}

#[test]
fn keeper_bound_timeout_resume_delivers_keeper_timeout() {
    let BoundFixture {
        setup,
        id: id_owned,
        keeper_pattern,
        keeper_watchdog,
        marker,
        job_watchdog,
        ready,
        gate: _,
    } = bound_fixture();
    let id = id_owned.as_str();
    // Shorten the keeper's post-crash bound for this test alone: the hook is
    // per-command environment, so nothing leaks into other tests. The keeper
    // inherits it because the session spawn sets only `FIBER_HOME`.
    let mut ask = setup.fiber(&[
        "ask",
        "--resume",
        id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    ask.current_dir(setup.workspace());
    ask.env("FIBER_KEEPER_BOUND_MS", "5000");
    let (mut child, session_watchdog) = spawn_watched(&mut ask);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    let started = event(&lines, "job_started", setup.deadline);
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    event(&lines, "tool_call_started", setup.deadline);
    let ready_pids = ready.wait(setup.deadline.start_wait());
    assert!(!ready_pids.is_empty(), "the held job reports its pid");
    let job_group = ready_pids[0];
    fakes::kill_group(group, "KILL").unwrap();
    assert!(!reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());

    // No wait on the keeper's socket here: with this bound the keeper may
    // have expired and unlinked it before a poll runs. The keeper's own
    // record below shows it took the log.
    let dir = session_dir(&setup, id);
    fakes::wait_until(
        "keeper-timeout job_completed to appear",
        setup.deadline.start_wait(),
        {
            let dir = dir.clone();
            move || {
                fs::read(dir.join("events.jsonl"))
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains("keeper_timeout"))
                    .unwrap_or(false)
            }
        },
    );
    let lines = complete_lines(&fs::read_to_string(dir.join("events.jsonl")).unwrap());
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .collect();
    assert_eq!(ends.len(), 1, "the bound writes one ending: {lines:?}");
    let end = &ends[0]["payload"];
    assert_eq!(end["job_id"], job);
    assert_eq!(end["status"], "failed");
    assert_eq!(end["error"]["code"], "keeper_timeout");
    for field in ["output_tail", "notice", "changed_by"] {
        assert!(
            end.get(field).is_none(),
            "keeper must not set {field}: {end}"
        );
    }
    assert!(
        fakes::group_empties(job_group, setup.deadline.start_wait()),
        "the timed-out job's process group is gone"
    );
    assert!(
        fakes::matching_exits(&keeper_pattern, setup.deadline.start_wait()),
        "no keeper process remains after the timeout exit"
    );
    script(&setup, "done.json", json!([{"text":"Recovered."}]));
    let resumed = run(
        &setup,
        &["ask", "--resume", id, "--model", "scripted/done.json", "go"],
    );
    assert!(
        resumed.status.success(),
        "resume after the timeout exit succeeds, it is not refused as Held: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed_lines =
        complete_lines(&String::from_utf8(resumed.stdout.clone()).unwrap_or_default());
    assert!(
        !resumed_lines
            .iter()
            .any(|line| line["payload"]["error"]["code"] == "session_held"),
        "the post-bound resume is not refused as Held"
    );
    let lines = complete_lines(&fs::read_to_string(dir.join("events.jsonl")).unwrap());
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["payload"]["job_id"], job);
    assert_eq!(ends[0]["payload"]["status"], "failed");
    assert_eq!(ends[0]["payload"]["error"]["code"], "keeper_timeout");
    let ended_at = lines
        .iter()
        .position(|line| line["kind"] == "job_completed")
        .unwrap();
    let resumed_at = lines
        .iter()
        .rposition(|line| line["kind"] == "fiber_started")
        .unwrap();
    assert!(
        ended_at < resumed_at,
        "docs/tools.md, \"The keeper\": keeper appends the job's end while the session is gone"
    );
    assert!(fakes::matching_exits(&marker, setup.deadline.start_wait()));
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
    job_watchdog.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn keeper_bound_holds_job_in_bound_without_resume() {
    let BoundFixture {
        setup,
        id: id_owned,
        keeper_pattern,
        keeper_watchdog,
        marker,
        job_watchdog,
        ready,
        gate,
    } = bound_fixture();
    let id = id_owned.as_str();
    // A long bound for this test alone: the in-bound observation lands well
    // before it. Per-command environment, so nothing leaks into other tests.
    let mut ask = setup.fiber(&[
        "ask",
        "--resume",
        id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    ask.current_dir(setup.workspace());
    ask.env("FIBER_KEEPER_BOUND_MS", "30000");
    let (mut child, session_watchdog) = spawn_watched(&mut ask);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    let started = event(&lines, "job_started", setup.deadline);
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    event(&lines, "tool_call_started", setup.deadline);
    assert!(!ready.wait(setup.deadline.start_wait()).is_empty());
    fakes::kill_group(group, "KILL").unwrap();
    assert!(!reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());

    // Observe the bind after the killed session has released its socket.
    // Do not subscribe: the keeper accepts and closes without protocol.
    let socket = setup.session_socket(id);
    wait_for_refuse_only_socket(setup.deadline.start_wait(), socket);
    // Inside the bound, with no resume: the keeper still runs and still
    // holds its job, and no ending is written yet.
    assert!(
        !fakes::matching(&keeper_pattern, setup.deadline.left() / 2)
            .unwrap()
            .is_empty(),
        "the keeper is still alive inside the bound"
    );
    assert!(
        !fakes::matching(&marker, setup.deadline.left() / 2)
            .unwrap()
            .is_empty(),
        "the held job is still running under the keeper inside the bound"
    );
    let dir = session_dir(&setup, id);
    let lines = complete_lines(&fs::read_to_string(dir.join("events.jsonl")).unwrap());
    assert!(
        !lines.iter().any(|line| line["kind"] == "job_completed"),
        "no job_completed is written before the bound: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line["payload"]["error"]["code"] == "keeper_timeout"),
        "no keeper_timeout is written before the bound"
    );
    // Release the held job so the keeper exits through its normal end and
    // the test leaves nothing behind; the bound never fires.
    let release = gate.clone();
    support::bounded(
        setup.deadline.start_wait(),
        "releasing the keeper job",
        move || fs::write(release, b"").unwrap(),
    );
    fakes::wait_until(
        "held job to complete after release",
        setup.deadline.start_wait(),
        {
            let dir = dir.clone();
            move || {
                fs::read(dir.join("events.jsonl"))
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains("job_completed"))
                    .unwrap_or(false)
            }
        },
    );
    assert!(
        fakes::matching_exits(&keeper_pattern, setup.deadline.start_wait()),
        "the keeper exits after its released job ends"
    );
    let lines = complete_lines(&fs::read_to_string(dir.join("events.jsonl")).unwrap());
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .collect();
    assert_eq!(ends.len(), 1, "the released job completes once: {lines:?}");
    assert_eq!(ends[0]["payload"]["job_id"], job);
    assert_eq!(ends[0]["payload"]["status"], "completed");
    assert!(
        ends[0]["payload"].get("error").is_none(),
        "the in-bound end is not a timeout: {}",
        ends[0]["payload"]
    );
    assert!(fakes::matching_exits(&marker, setup.deadline.start_wait()));
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
    job_watchdog.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_session_with_no_command_starts_no_keeper() {
    let setup = configured();
    let id = "s_0123456789abcdef";
    let keeper_pattern = format!("keeper --id {id}");
    let keeper_watchdog = Watchdog::matching(&keeper_pattern);
    let workspace = setup.workspace();
    let mut command = setup.fiber(&[
        "session",
        "--id",
        id,
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    command.current_dir(&workspace);
    let (mut child, session_watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    event(&lines, "fiber_started", setup.deadline);
    let client = Socket::connect(setup.deadline, &setup.session_socket(id));
    let sub = subscribe_direct(&client);
    client.send(r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hello"}]}}"#);
    assert_eq!(
        recv_answer(&client, "c_prompt", "prompt accepted")["kind"],
        "command_accepted"
    );
    let live = until_after(&client, &sub, "text-only turn completion", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(live.iter().any(|line| line["kind"] == "turn_completed"));
    assert!(
        fakes::matching(&keeper_pattern, setup.deadline.left() / 2)
            .unwrap()
            .is_empty()
    );
    drop(Socket::connect(setup.deadline, &setup.session_socket(id)));
    client.send(r#"{"id":"c_close","command":"close"}"#);
    assert_eq!(
        recv_answer(&client, "c_close", "close accepted")["kind"],
        "command_accepted"
    );
    assert!(reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
}
