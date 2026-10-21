//! A Fiber home and extension sources in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![allow(dead_code, reason = "each test file uses a different part")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::events::{OfferDecision, OfferedItem, OfferedKind};
use contract::repository::{Decided, RepositoryCode};
use contract::shapes::Failure;
use contract::signing::{SignRequest, Signer};
use extensions::{
    CopyHold, CredentialPair, Error, Fetch, Index, LuaExtension, LuaProvider, Origin, RepoItem,
    Request, SessionOffer, Store, declared_items, hash, observe_load, plan,
};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::{Value, json};

#[allow(unused_imports, reason = "each test file uses a different part")]
pub(crate) use fakes::write_record;

/// How far each round drives the fake clock: a day past any install or git
/// bound, so the deadline, the grace and the drain all elapse.
const FAR: Duration = Duration::from_secs(24 * 3600);

/// Bounds the fake-clock jumps while driving a stalled calibration.
const ROUNDS: u32 = 30;

/// One bounded wait for the run's answer or park signal.
pub(crate) const ROUND: Duration = Duration::from_millis(200);

/// How long each hang guard below waits for the whole wait: one wall-clock
/// deadline per wait, made before its first check. The per-iteration bounded
/// receives are only the poll pause between checks, never a fresh deadline.
///
/// Waits until the stalled run parks on the clock or answers, returning an
/// early answer at once. A test moves fake time only once the run is waiting
/// on it, past its own clock check; a run that answers without ever waiting
/// fails on its own answer, not on a jump past bounds it never computed.
#[track_caller]
fn wait_parked<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    let bound = Deadline::after(Duration::from_secs(10));
    loop {
        if !clock.parked().is_empty() {
            return None;
        }
        if bound.left().is_zero() {
            break;
        }
        match Deadline::after(ROUND).recv(done) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run parks on the clock");
}

/// Waits until the process holding `stall` on its command line has outlived
/// a bounded wait, returning an early answer at once. A process seen running
/// across the wait is stuck, not starting: stopping a starter reports a
/// timeout the stall never caused, so the clock moves only after the second
/// sighting. A stall that answers instead fails on its own answer.
#[track_caller]
fn await_stuck<T: Send>(done: &std::sync::mpsc::Receiver<T>, stall: &str) -> Option<T> {
    let bound = Deadline::after(Duration::from_secs(10));
    loop {
        if !fakes::matching(stall).unwrap().is_empty() {
            // Up: still up after a bounded wait means stuck, not starting;
            // an answer meanwhile ends this at once.
            match Deadline::after(ROUND).recv(done) {
                Ok(done) => return Some(done),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            }
            if !fakes::matching(stall).unwrap().is_empty() {
                return None;
            }
        }
        if bound.left().is_zero() {
            break;
        }
        match Deadline::after(ROUND).recv(done) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled process runs");
}

/// Waits for the run's next park after a clock advance, or returns an early
/// answer. A later clock advance cannot collapse the park's boundary.
#[track_caller]
fn wait_parked_since<T: Send>(
    clock: &fakes::clock::FakeClock,
    mark: &fakes::clock::Mark,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    let bound = Deadline::after(Duration::from_secs(10));
    loop {
        if clock.await_any_parked_since(mark, fakes::Deadline::after(ROUND)) {
            return None;
        }
        match done.try_recv() {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
        if bound.left().is_zero() {
            break;
        }
    }
    panic!("the stalled run parks on the clock after an advance");
}

/// Drives `clock` until the stalled run on `done` answers: the run first
/// parks on the clock, and the process holding `stall` on its command line
/// first proves it is stuck, so whatever instant the run computes its bounds
/// at, the next jump lands past them. Each later jump waits for a fresh park
/// acknowledgement, so the deadline, grace and drain cannot collapse into
/// one instant. An early answer ends the rounds; a run that never answers
/// fails, naming the stall.
#[track_caller]
pub(crate) fn drive<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: std::sync::mpsc::Receiver<T>,
    stall: &str,
) -> T {
    // Both acknowledgements precede the first jump: stopping a starter on
    // the way up reports a timeout the stall never caused.
    if let Some(done) = wait_parked(clock, &done) {
        return done;
    }
    if let Some(done) = await_stuck(&done, stall) {
        return done;
    }
    for _ in 0..ROUNDS {
        match done.try_recv() {
            Ok(done) => return done,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
        // A stall driver: the callers own the bounds, so no single park is
        // knowable here; each jump goes a day past any install or git bound
        // and only the run's answer is asserted (ruling 6263191610).
        let mark = clock.advance_marked(FAR);
        if let Some(done) = wait_parked_since(clock, &mark, &done) {
            return done;
        }
    }
    panic!("the stalled run returns");
}

pub(crate) struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extensions");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files. When the manifest has no `providers` key, the
    /// matching declaration is derived from the data's provider names and
    /// their models' distinct `base_url`s, so the helper builds a
    /// well-formed package.
    pub(crate) fn source(&self, dir: &str, manifest: &Value, providers: &[Value]) -> PathBuf {
        let mut manifest = manifest.clone();
        if manifest.get("providers").is_none()
            && let Some(into) = manifest.as_object_mut()
        {
            into.insert("providers".to_owned(), declared_map(providers));
        }
        let path = self.root.path().join("src").join(dir);
        write(&path.join("extension.json"), &manifest.to_string());
        for provider in providers {
            let name = provider["name"].as_str().unwrap();
            write(
                &path.join("providers").join(format!("{name}.json")),
                &provider.to_string(),
            );
        }
        path
    }
}

pub(crate) fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

pub(crate) fn manifest(name: &str) -> Value {
    json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 })
}

/// A provider with these model ids, each on `openai-responses`.
pub(crate) fn provider(name: &str, ids: &[&str]) -> Value {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    json!({ "name": name, "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models })
}

/// The `providers` declaration matching these data files: each provider
/// name to its models' distinct `base_url`s, in first-seen order.
fn declared_map(providers: &[Value]) -> Value {
    let mut map = serde_json::Map::new();
    for data in providers {
        let Some(name) = data.get("name").and_then(Value::as_str) else {
            continue;
        };
        let mut urls = Vec::new();
        for model in data
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(url) = model.get("base_url").and_then(Value::as_str)
                && !urls.contains(&url)
            {
                urls.push(url);
            }
        }
        map.insert(
            name.to_owned(),
            Value::Array(urls.into_iter().map(Value::from).collect()),
        );
    }
    Value::Object(map)
}

/// Installs the extension in `source` the way `fiber extension install <path>` does
/// and returns its name.
pub(crate) fn install(home: &Path, source: &Path, fiber: &str) -> Result<String, Error> {
    let names = plan(
        home,
        &Request::Path(source.into()),
        fiber,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )?
    .commit()?;
    Ok(names.into_iter().next().unwrap())
}

/// Installs `fiber.test/<short>` with `fields` merged into its manifest and
/// the entry script `init`, and returns its installed directory.
pub(crate) fn install_lua(setup: &Setup, short: &str, fields: &Value, init: &str) -> PathBuf {
    let name = format!("fiber.test/{short}");
    let mut listed = manifest(&name);
    if let Value::Object(extra) = fields
        && let Value::Object(into) = &mut listed
    {
        for (key, value) in extra {
            into.insert(key.clone(), value.clone());
        }
    }
    let source = setup.source(short, &listed, &[]);
    write(&source.join("init.lua"), init);
    install(&setup.home(), &source, "0.1.0").unwrap();
    setup
        .home()
        .join("extensions")
        .join(config::dir_name(&name))
}

/// A config over the test home and workspace, with these overrides.
pub(crate) fn config(setup: &Setup, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

/// A Lua provider whose `models()` is `models_run`, with a `credential`
/// function: what most `add_lua` tests use, so the credential gate lets
/// `models()` run.
pub(crate) fn lua_named(
    setup: &Setup,
    dir: &str,
    provider: &str,
    models_run: &str,
) -> Arc<LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("providers").join(format!("{provider}.lua")),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"test-token\", expires_at = 1893456000 }} end }}, \
             models = {{ timeout = 1000, run = function() return {models_run} end }} }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new(
        "acme-ext",
        ext,
        setup.home(),
        FakeClock::new(),
    ));
    LuaProvider::new(extension, provider)
}

/// A test-local provider `p` on `clock`: `credential` and `sign` run
/// `credential_run` and `sign_run`, each absent when its option is `None`.
pub(crate) fn script_provider(
    setup: &Setup,
    clock: Arc<FakeClock>,
    credential_run: Option<&str>,
    sign_run: Option<&str>,
) -> Arc<LuaProvider> {
    let mut spec = Vec::new();
    if let Some(run) = credential_run {
        spec.push(format!(
            "credential = {{ timeout = 60000, run = function() return {run} end }}"
        ));
    }
    if let Some(run) = sign_run {
        spec.push(format!(
            "sign = {{ timeout = 60000, run = function(request) return {run} end }}"
        ));
    }
    let dir = setup.home().join("ext");
    write(
        &dir.join("providers/p.lua"),
        &format!("fiber.provider(\"p\", {{ {} }})\n", spec.join(", ")),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock));
    LuaProvider::new(extension, "p")
}

/// The pair `credential`/`label`.
pub(crate) fn pair(credential: &str, label: &str) -> CredentialPair {
    CredentialPair {
        credential: credential.to_owned(),
        label: label.to_owned(),
    }
}

/// The value of `name` in `headers`, case-sensitively.
pub(crate) fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

/// How long a sign waits before it fails the test instead of hanging it.
const SIGN_WITHIN: Duration = Duration::from_secs(5);

/// Signs one request with `headers` already on it, on its own thread under
/// [`SIGN_WITHIN`], so a call that never returns fails the test instead of
/// hanging it.
#[track_caller]
pub(crate) fn sign_with(
    signer: &Arc<dyn Signer>,
    headers: &[(String, String)],
) -> Result<Vec<(String, String)>, contract::signing::Error> {
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let body = br#"{"model":"m"}"#.to_vec();
    let signer = Arc::clone(signer);
    let owned: Vec<(String, String)> = headers.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &owned,
            body: &body,
        }))
    });
    match Deadline::after(SIGN_WITHIN).recv(&rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the sign did not return within {SIGN_WITHIN:?}"),
    }
}

/// Repositories by name, in the setup's directory.
pub(crate) struct Repos<'a> {
    setup: &'a Setup,
    paths: BTreeMap<String, PathBuf>,
}

impl<'a> Repos<'a> {
    pub(crate) fn new(setup: &'a Setup) -> Self {
        Self {
            setup,
            paths: BTreeMap::new(),
        }
    }

    /// A repository `host/owner/repo` holding an extension in `dir` (empty
    /// for its root), committed and tagged `tag`. A later call commits
    /// again.
    pub(crate) fn tag(
        &mut self,
        repo: &str,
        dir: &str,
        tag: &str,
        manifest: &Value,
        extra: &[(&str, &str)],
    ) {
        let path = self.setup.root().join("repos").join(repo.replace('/', "-"));
        if !path.exists() {
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "--quiet"]);
        }
        let ext = path.join(dir);
        write(&ext.join("extension.json"), &manifest.to_string());
        write(
            &ext.join("providers/acme.json"),
            &provider("acme", &["m1"]).to_string(),
        );
        for (file, text) in extra {
            write(&ext.join(file), text);
        }
        git(&path, &["add", "."]);
        git(
            &path,
            &["commit", "--quiet", "--allow-empty", "-m", "release"],
        );
        if !tag.is_empty() {
            git(&path, &["tag", tag]);
        }
        self.paths.insert(repo.to_owned(), path);
    }

    pub(crate) fn commit(&self, repo: &str, tag: &str) -> String {
        git(
            self.paths
                .get(repo)
                .unwrap_or_else(|| panic!("no such repo: {repo}")),
            &["rev-parse", &format!("{tag}^{{commit}}")],
        )
    }

    pub(crate) fn origin(&self) -> Origin {
        let paths = self.paths.clone();
        Origin::new("git", move |repo| {
            // Every fetch resolves its URL here first: the bump proves git
            // work started, re-arming the stall bound in `on_worker`.
            GIT_PROGRESS.fetch_add(1, Ordering::Relaxed);
            match paths.get(repo) {
                Some(path) => format!("file://{}", path.display()),
                None => "file:///nonexistent-repository".into(),
            }
        })
    }
}

/// `require("go_<name>")` in `dir` signals that the callback has started:
/// the loader runs the registered observer before it opens the file, the
/// observer reports it, and the module reads empty.
pub(crate) fn go_module(dir: &Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    std::fs::File::create(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    observe_load(
        path.canonicalize().unwrap(),
        Box::new(move || match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }),
    );
    rx
}

/// Pinned-copy fixtures for `pinned_depends` (resolution) and
/// `pinned_copy` (transaction, completion, legacy, confinement): a
/// repository workspace declaring one package, approved through the public
/// [`Store`] API with dependencies from local tagged repositories.
pub(crate) const FIBER: &str = "0.1.0";
pub(crate) fn project() -> ProjectKey {
    ProjectKey::new("pin").unwrap()
}
pub(crate) fn named(name: &str, depends: &[(&str, &str)]) -> Value {
    let mut m = manifest(name);
    m.as_object_mut().unwrap().insert(
        "depends".into(),
        json!(depends.iter().copied().collect::<BTreeMap<_, _>>()),
    );
    m
}
/// The manifest `name` with `depends` and this install step.
pub(crate) fn installing(name: &str, depends: &[(&str, &str)], step: &str) -> Value {
    let mut m = named(name, depends);
    m.as_object_mut()
        .unwrap()
        .insert("install".into(), json!(["sh", "-c", step]));
    m
}
/// The workspace declares the package at `pkg` with this manifest, listed
/// by git as a repository package is.
pub(crate) fn declare(setup: &Setup, root: &Value) {
    let ws = setup.workspace();
    if !ws.join(".git").exists() {
        git(&ws, &["init", "--quiet"]);
    }
    write(&ws.join("pkg/extension.json"), &root.to_string());
    write(&ws.join("pkg/init.lua"), "-- entry\n");
    write(
        &ws.join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "pkg"}]}).to_string(),
    );
}
pub(crate) fn store(setup: &Setup, repos: &Repos, clock: &Arc<FakeClock>) -> Store {
    Store::new(&setup.home(), &project(), Arc::clone(clock) as Arc<_>).fetching(Fetch::new(
        repos.origin(),
        FIBER,
        Arc::clone(clock) as Arc<_>,
    ))
}
/// The declared extension and its content hash.
pub(crate) fn item(setup: &Setup, clock: &Arc<FakeClock>) -> (extensions::RepoItem, String) {
    let items = declared_items(&setup.workspace(), &**clock).unwrap();
    let found = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Extension)
        .unwrap();
    let digest = hash(&mut Index::load(&setup.home()), &found).unwrap();
    (found, digest)
}
/// How long a blocking store operation may stay silent (no new git work)
/// before the test fails instead of hanging.
const STORE_OPERATION_WITHIN: Duration = Duration::from_secs(10);

/// Git work started across the test process, bumped whenever a repository
/// URL is resolved for a fetch. Nextest runs each test in its own process,
/// so the count is per test: a monotonic hint of progress, guarding no
/// data, which is why `Ordering::Relaxed` is enough everywhere it is read.
pub(crate) static GIT_PROGRESS: AtomicU64 = AtomicU64::new(0);

/// Stall-bound slices `on_worker_within` has waited through without an
/// answer, so a test can hold work for a whole slice by waiting on this
/// count instead of on a clock.
pub(crate) static STALL_SLICES: AtomicU64 = AtomicU64::new(0);

/// Runs one blocking store operation on a worker under a stall bound:
/// silence (no new git work) for the whole bound fails the test instead of
/// hanging it, while slow git work that keeps starting fetches runs as long
/// as it progresses. Joining after the result proves the worker has
/// returned before the test continues.
pub(crate) fn on_worker_within<T: Send + 'static>(
    bound: Duration,
    waited_for: &'static str,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (done, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done.send(operation()).unwrap();
    });
    // A stall bound, not a total bound: each `ROUND` slice waits for the
    // result then checks for progress, and progress pushes the failure
    // out. Silence is counted in slices, never read off a clock, so a slow
    // fetch that keeps starting work never trips it.
    let mut seen = GIT_PROGRESS.load(Ordering::Relaxed);
    let mut silent = Duration::ZERO;
    loop {
        match Deadline::after(ROUND).recv(&result) {
            Ok(value) => {
                worker.join().unwrap();
                return value;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                STALL_SLICES.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the sender for {waited_for} hung up before sending")
            }
        }
        let progress = GIT_PROGRESS.load(Ordering::Relaxed);
        if progress != seen {
            seen = progress;
            silent = Duration::ZERO;
        } else {
            silent += ROUND;
            if silent >= bound {
                panic!("waited until the deadline for {waited_for}");
            }
        }
    }
}

/// Runs one blocking store operation on a worker under the operation bound:
/// a stall fails the test instead of hanging it. Joining after the result
/// proves the worker has returned before the test continues.
fn on_worker<T: Send + 'static>(
    waited_for: &'static str,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    on_worker_within(STORE_OPERATION_WITHIN, waited_for, operation)
}

/// Store operations that can block on the filesystem, git or install steps.
/// Tests use these methods instead of calling the synchronous API directly.
pub(crate) trait StoreOperations {
    fn approve_on_worker(&self, item: &RepoItem, hash: &str) -> Result<(), Error>;
    fn ensure_copy_on_worker(&self, item: &RepoItem, hash: &str) -> Result<CopyHold, Error>;
}

impl StoreOperations for Store {
    fn approve_on_worker(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        let store = self.clone();
        let item = item.clone();
        let hash = hash.to_owned();
        on_worker("the approval", move || store.approve(&item, &hash))
    }

    fn ensure_copy_on_worker(&self, item: &RepoItem, hash: &str) -> Result<CopyHold, Error> {
        let store = self.clone();
        let item = item.clone();
        let hash = hash.to_owned();
        on_worker("the copy", move || store.ensure_copy(&item, &hash))
    }
}

/// Approves the declared extension with its dependencies fetched from
/// `repos`, and returns its content hash. The approval blocks on git and
/// install steps, so it runs on a worker under one deadline: a stall fails
/// the test instead of hanging it.
pub(crate) fn approve(setup: &Setup, repos: &Repos) -> String {
    let clock = FakeClock::new();
    let store = store(setup, repos, &clock);
    let (found, digest) = item(setup, &clock);
    let waiting = digest.clone();
    on_worker("the approval", move || store.approve(&found, &waiting)).unwrap();
    digest
}
/// Runs one offer decision on a worker under one deadline: an approval
/// blocks on git and install steps, so a stall fails the test instead of
/// hanging it. `Never` and `Skip` never fetch; only `Approve` needs this,
/// but every decision reads the same way.
pub(crate) fn decide_on_worker(
    offer: &SessionOffer,
    item: &OfferedItem,
    decision: OfferDecision,
) -> Result<Decided, Failure> {
    let offer = offer.clone();
    let item = item.clone();
    on_worker("the decision", move || offer.decide(&item, decision))
}
/// Lists the pending offers on a worker under one deadline: gathering
/// declarations reads the workspace, so a stall fails the test instead of
/// hanging it.
pub(crate) fn unapproved_on_worker(
    offer: &SessionOffer,
) -> Result<Vec<contract::repository::Unapproved>, Failure> {
    let offer = offer.clone();
    on_worker("the offers", move || offer.unapproved())
}
/// The content hash of the declared extension, without approving.
pub(crate) fn digest_of(setup: &Setup) -> String {
    let clock = FakeClock::new();
    item(setup, &clock).1
}
pub(crate) fn depends_home(setup: &Setup, digest: &str) -> PathBuf {
    setup
        .home()
        .join("pinned")
        .join(digest)
        .join(".fiber-depends")
}
pub(crate) fn dep_dir(setup: &Setup, digest: &str, dep: &str) -> PathBuf {
    depends_home(setup, digest)
        .join("extensions")
        .join(config::dir_name(dep))
}
/// The installed version of `dep` inside the pinned copy: the tag it was
/// fetched at, as its install record says (a package's manifest keeps its
/// own version across tags).
pub(crate) fn dep_version(setup: &Setup, digest: &str, dep: &str) -> String {
    dep_record(setup, digest, dep)
        .get("version")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}
/// The install record of `dep` inside the pinned copy.
pub(crate) fn dep_record(setup: &Setup, digest: &str, dep: &str) -> Value {
    let text = fs::read_to_string(dep_dir(setup, digest, dep).join(".fiber.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}
/// The names installed inside the pinned copy, in order; dotfiles are
/// the plan's staging directories, never installs.
pub(crate) fn dep_names(setup: &Setup, digest: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(depends_home(setup, digest).join("extensions"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.retain(|n| !n.starts_with('.'));
    names.sort();
    names
}
/// The approval file for `digest` under the test project.
pub(crate) fn approval(setup: &Setup, digest: &str) -> PathBuf {
    setup
        .home()
        .join("projects")
        .join("pin")
        .join("approvals")
        .join(digest)
}
pub(crate) fn ready(setup: &Setup, digest: &str) -> PathBuf {
    setup.home().join("pinned").join(format!("{digest}.ready"))
}
pub(crate) fn copy(setup: &Setup, digest: &str) -> PathBuf {
    setup.home().join("pinned").join(digest)
}
/// Approves a counting root with one dependency, and returns its hash.
pub(crate) fn counting_root(setup: &Setup, repos: &mut Repos) -> String {
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(
        setup,
        &installing(root, &[(helper, "1.0")], "echo x >> counter"),
    );
    approve(setup, repos)
}
pub(crate) fn counter(setup: &Setup, digest: &str) -> String {
    fs::read_to_string(copy(setup, digest).join("counter")).unwrap_or_default()
}
/// A file outside Fiber home with sentinel bytes, and its directory.
pub(crate) fn outside(root: &Path) -> (PathBuf, PathBuf) {
    let dir = root.join("outside");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.txt");
    fs::write(&target, "untouched\n").unwrap();
    (dir, target)
}
pub(crate) fn outside_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
