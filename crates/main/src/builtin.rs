//! The built-in tools one session registers (`docs/architecture.md`, "Tool
//! seam"). Registration order is name order, so the tool definitions in a
//! request stay byte-identical across sessions with the same inputs
//! (`docs/prompt-cache.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::diag::Timing;
use contract::events::{ToolInfo, ToolSource, ToolState};
use contract::search::SearchBackend;
use contract::shapes::Failure;
use contract::tool::Tool;

use crate::failed;

/// The loop's `(who, tool)` pairs, the infos the `tools` command answers
/// with, the driver shell [`doors::Session::shell`] runs, and what a handoff
/// runs to clear what the file tools have seen.
type SessionTools = (
    Vec<(String, Arc<dyn Tool>)>,
    Vec<ToolInfo>,
    Arc<dyn Tool>,
    Arc<dyn Fn() + Send + Sync>,
    Arc<dyn contract::images::Images>,
);

/// `ask_user`, `delegate_models`, `delegate_spawn`, `edit`, `handoff`, `name_session`, `read`, `session_search`, `shell`,
/// `skill`, `web_fetch`, `write` and `jobs`, each registered by `builtin`, and
/// `web_search` as `web_search` chose it: the hosted search of the session's
/// model when it has one, else Fiber's own over the installed backend, else
/// nothing (`docs/tools.md`, "Hosted by the provider" and "Fiber's own, over
/// a backend"). `session_search` searches the logs under `home`, Fiber
/// home, and counts a session as this project's when `doors::project` gives
/// its workspace the same identity as `workspace`, as resume looks a
/// session up. `read`, `write` and `edit`
/// share one session's file state, which a handoff forgets, take `locks`,
/// the session's per-path lock, which an extension's `host.fs` shares, and
/// `read` runs the image child (`fiber image`) into `artifacts`, the session's
/// `artifacts/` directory, where `web_fetch` saves the PDFs and images it
/// downloads; the model's `shell` moves commands into `jobs`, which the
/// `jobs` tool lists, waits on and stops. The driver shell runs in the
/// foreground only. `fiber` is the executable recorded at process startup.
#[allow(
    clippy::too_many_arguments,
    reason = "one session's built-ins need Fiber home and the recorded executable alongside the session's own paths"
)]
pub(crate) fn builtin(
    fiber: PathBuf,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
) -> Result<SessionTools, Failure> {
    // The current production caller and the unit tests: no image-child
    // timing. The debug path forwards its timing through `builtin_timed`
    // once that composition lands; then this entry becomes its test-only
    // form.
    builtin_timed(
        fiber, home, workspace, artifacts, clock, jobs, locks, web_search, delegates, skills, None,
    )
}

/// One session's built-in tools with an optional image-child timing: the
/// same registration as [`builtin`], timing each entered image or PDF
/// count/cut call as `picture.run` when `timing` is present. The timing is
/// applied to both image drivers, the file tools' child and the shared
/// standalone child, before either is built; `None` keeps today's
/// behaviour with no callable invoked.
#[allow(
    clippy::too_many_arguments,
    reason = "one session's built-ins need Fiber home and the recorded executable alongside the session's own paths"
)]
pub(crate) fn builtin_timed(
    fiber: PathBuf,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
    timing: Option<Timing>,
) -> Result<SessionTools, Failure> {
    let mut files = tools::Files::with_locks(workspace.to_path_buf(), Arc::clone(locks));
    let mut images = tools::ImageChild::new(fiber.clone(), artifacts.to_path_buf());
    if let Some(timing) = timing {
        files = files.with_timing(timing.clone());
        images = images.with_timing(timing);
    }
    let files = Arc::new(files.with_images(fiber.clone(), artifacts.to_path_buf()));
    let images: Arc<dyn contract::images::Images> = Arc::new(images);
    let moves: Arc<dyn contract::jobs::Jobs> = jobs.clone();
    // Only `fiber ask` reaches here, new or resumed: a non-interactive run.
    // The model's shell and the driver shell spill their output past 1 MiB
    // into the session's `artifacts/`; the driver has no hook.
    let shell = tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
        .with_search(fiber.clone())
        .with_jobs(moves)
        .with_artifacts(artifacts.to_path_buf())
        .non_interactive();
    let driver = Arc::new(
        tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
            .with_search(fiber)
            .with_artifacts(artifacts.to_path_buf()),
    );
    let mut built = vec![
        registered(tools::AskUser)?,
        registered(delegates.models())?,
        registered(delegates.tool())?,
        registered(files.edit())?,
        registered(tools::Handoff)?,
        registered(tools::NameSession)?,
        registered(files.read())?,
        registered(tools::SessionSearch::new(Arc::new(log::SessionScan::new(
            home,
            workspace,
            Arc::new(doors::project),
        ))))?,
        registered(shell)?,
        registered(tools::Skill::new(skills))?,
        registered(tools::WebFetch::new(
            artifacts.to_path_buf(),
            Arc::clone(clock),
        ))?,
        registered(files.write())?,
        registered(jobs::JobsTool::new(Arc::clone(jobs)))?,
    ];
    // What `web_search` chose for the session's preamble: the hosted
    // search, Fiber's own over the backend, or nothing. It is fixed when
    // the session's preamble is built (`docs/tools.md`, "Hosted by the
    // provider").
    if let Some(given) = web_search {
        built.push(given);
    }
    let mut pairs = Vec::new();
    let mut infos = Vec::new();
    for (tool, info) in built {
        pairs.push((String::from("builtin"), tool));
        infos.push(info);
    }
    let forget: Arc<dyn Fn() + Send + Sync> = Arc::new(move || files.forget());
    Ok((pairs, infos, driver, forget, images))
}

/// The provider's hosted search of type `kind`, and what the `tools`
/// command answers for it (`docs/tools.md`, "Hosted by the provider").
pub(crate) fn hosted(kind: &str) -> Result<(Arc<dyn Tool>, ToolInfo), Failure> {
    registered(tools::HostedSearch::new(kind.to_owned()))
}

/// What `web_search` chose for the session: the declared tool and what
/// the `tools` command answers for it, if any.
type Chosen = Option<(Arc<dyn Tool>, ToolInfo)>;

/// The `web_search` the session declares: the model's hosted search when
/// it has one, else Fiber's own tool over the chosen backend, else nothing
/// (`docs/tools.md`, "web_search": the model sees one tool, whichever way
/// it runs). At most one `web_search` is registered at any time.
pub(crate) fn web_search(
    kind: Option<&str>,
    backend: Option<&Arc<dyn SearchBackend>>,
) -> Result<Chosen, Failure> {
    if let Some(kind) = kind {
        return hosted(kind).map(Some);
    }
    let Some(backend) = backend else {
        return Ok(None);
    };
    registered(tools::BackendSearch::new(Arc::clone(backend))).map(Some)
}

/// One tool and what the `tools` command answers for it.
fn registered(tool: impl Tool + 'static) -> Result<(Arc<dyn Tool>, ToolInfo), Failure> {
    let info = info(&tool, ToolSource::Builtin)?;
    Ok((Arc::new(tool), info))
}

/// What the `tools` command answers for `tool`, declared in full, which
/// `source` registered.
pub(crate) fn info(tool: &dyn Tool, source: ToolSource) -> Result<ToolInfo, Failure> {
    let definition = tool.definition();
    let encoded = serde_json::to_vec(&definition)
        .map_err(|error| failed(ErrorCode::IoFailed, format!("a tool definition: {error}")))?;
    Ok(ToolInfo {
        name: definition.name,
        source,
        state: ToolState::Full,
        bytes: u64::try_from(encoded.len()).unwrap_or(u64::MAX),
        tokens: None,
    })
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_budget_tests.rs"]
mod tool_budget_tests;

/// `builtin_timed` wires one timing into both image drivers: the file
/// tools' child, reached through the `read` tool, and the shared
/// standalone child. A stub `fiber` answers both, so no image code runs.
#[cfg(test)]
mod timed_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use contract::diag::{Op, Timing};
    use contract::tool::Cancel;

    struct Never;

    impl Cancel for Never {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
    }

    struct Quiet;

    impl contract::emit::Emit for Quiet {
        fn emit(&self, _event: &contract::events::Event) {}
    }

    /// Counts timing calls and closed scopes.
    struct Probe {
        calls: AtomicUsize,
        closed: AtomicUsize,
    }

    struct Scope {
        probe: Arc<Probe>,
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            self.probe.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn timing(probe: &Arc<Probe>) -> Timing {
        let probe = Arc::clone(probe);
        Arc::new(
            move |op: Op, tool: Option<&str>, _started: Option<std::time::Instant>| {
                assert_eq!(op, Op::PictureRun);
                assert_eq!(tool, None);
                probe.calls.fetch_add(1, Ordering::SeqCst);
                Some(Box::new(Scope {
                    probe: Arc::clone(&probe),
                }) as Box<dyn Send>)
            },
        )
    }

    /// How long one builtin tool call may take: starting a child can stall
    /// for seconds, and a hang fails the test instead of hanging it.
    const CALL_WITHIN: Duration = Duration::from_secs(10);

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    /// The `builtin_timed` session over a stub `fiber` that answers image
    /// calls from this invocation's stem.
    #[allow(
        clippy::type_complexity,
        reason = "one test session: its directory, tools and shared images"
    )]
    fn session(
        timing: Option<Timing>,
    ) -> (
        fakes::TempDir,
        Vec<(String, Arc<dyn contract::tool::Tool>)>,
        Arc<dyn contract::images::Images>,
    ) {
        let root = fakes::TempDir::new("fiber-builtin-timed");
        std::fs::write(root.path().join("a.png"), PNG).expect("the image");
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let fiber = fakes::script(
            root.path(),
            "fiber-stub",
            "cat >/dev/null; printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\"",
        );
        let jobs = jobs::Registry::new(
            root.path().join("artifacts"),
            Arc::clone(&clock),
            Arc::new(fakes::Recorder::default()),
        );
        let (tools, _infos, _driver, _forget, images) = super::builtin_timed(
            fiber,
            &root.path().join("home"),
            root.path(),
            &root.path().join("artifacts"),
            &clock,
            &jobs,
            &Arc::new(tools::PathLocks::new()),
            None,
            &super::tests::delegates(),
            super::tests::skills(),
            timing,
        )
        .expect("the built-ins");
        (root, tools, images)
    }

    fn read_tool(
        tools: &[(String, Arc<dyn contract::tool::Tool>)],
    ) -> Arc<dyn contract::tool::Tool> {
        let (_, read) = tools
            .iter()
            .find(|(_, tool)| tool.definition().name == "read")
            .expect("read is registered");
        Arc::clone(read)
    }

    fn read_output(read: Arc<dyn contract::tool::Tool>) -> contract::tool::Output {
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "path".to_owned(),
            serde_json::Value::String("a.png".to_owned()),
        );
        fakes::within("the read call", CALL_WITHIN, move || {
            read.run(&arguments, &Never, &Quiet)
        })
    }

    #[test]
    fn timed_files_and_images_share_one_timing() {
        let probe = Arc::new(Probe {
            calls: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
        });
        let (_root, tools, images) = session(Some(timing(&probe)));

        let output = read_output(read_tool(&tools));
        assert_eq!(output.error, None);
        assert!(
            output
                .content
                .iter()
                .any(|part| matches!(part, contract::shapes::ContentPart::Image { .. })),
            "{output:?}"
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        assert_eq!(probe.closed.load(Ordering::SeqCst), 1);

        let bytes = PNG.to_vec();
        let reference = fakes::within("the images call", CALL_WITHIN, move || {
            images.process(&bytes, &Never)
        });
        let reference = reference.expect("the image");
        assert_eq!(reference.width, 1);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
        assert_eq!(probe.closed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn untimed_builtin_matches_with_no_timing_calls() {
        let probe = Arc::new(Probe {
            calls: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
        });
        let (_root, tools, images) = session(None);
        let timed_names: Vec<String> = {
            let (_timed_root, timed_tools, _) = session(Some(timing(&probe)));
            timed_tools
                .iter()
                .map(|(_, tool)| tool.definition().name)
                .collect()
        };

        let output = read_output(read_tool(&tools));
        assert_eq!(output.error, None);
        assert!(
            output
                .content
                .iter()
                .any(|part| matches!(part, contract::shapes::ContentPart::Image { .. })),
            "{output:?}"
        );
        let bytes = PNG.to_vec();
        let reference = fakes::within("the images call", CALL_WITHIN, move || {
            images.process(&bytes, &Never)
        })
        .expect("the image");
        assert_eq!(reference.width, 1);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0, "no callable runs");
        let names: Vec<String> = tools
            .iter()
            .map(|(_, tool)| tool.definition().name)
            .collect();
        assert_eq!(names, timed_names, "the same registration");
    }
}
