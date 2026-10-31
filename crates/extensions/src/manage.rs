//! `fiber extension install` and `fiber extension update` (`docs/extensions.md`, "Installing"): a
//! [`Plan`] fetches, resolves and checks everything first, and its commit
//! puts everything in place and runs each install step at its final path,
//! or nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use config::{Manifest, ProviderData};
use contract::clock::Clock;

use crate::Error;
use crate::git::{Origin, full_name, split};
use crate::install::{Paths, Provenance, Record, Scratch, commit_all, io, remove, slug, stage};
use crate::installed::{Damaged, Installed, Lock, lock, read};
use crate::prepare::{Observe, prepare};
use crate::resolve::{meets, newest, newest_within, pick, root_meets};

/// What is asked for.
pub enum Request {
    /// The extension in a local directory.
    Path(PathBuf),
    /// A name, or a first-party short name.
    Install(String),
    /// An installed extension, moved to its newest version.
    Update(String),
    /// One installed extension, as the no-name update's member for it:
    /// moved to its newest tag within the major version every extension
    /// that needs it accepts, recording that version as a floor without
    /// becoming a person request (`docs/extensions.md`, "Versions").
    UpdateAll(String),
    /// An installed extension, installed again: from its record's source
    /// and commit, or, when its directory is damaged, the name as given
    /// at its newest version (`docs/extensions.md`, "Installing").
    Reinstall(String),
}

/// One extension an install will put in place.
pub struct Item {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Where it comes from.
    pub provenance: Provenance,
    /// For an update, what changed since the installed commit.
    pub changes: Option<String>,
    /// Its manifest.
    pub manifest: Manifest,
    /// The providers it registers.
    pub providers: Vec<ProviderData>,
    paths: Paths,
}

impl Item {
    /// Where it comes from, as shown to the person: a path, or its name.
    pub fn source(&self) -> String {
        match &self.provenance {
            Provenance::Path(path) => path.display().to_string(),
            Provenance::Git { .. } => self.name.clone(),
        }
    }

    /// The staged copy of its files, which is what will be installed.
    pub fn staged(&self) -> &Path {
        &self.paths.fresh
    }

    /// What it carries, one line each ([`carries`]).
    pub fn carries(&self) -> Vec<String> {
        carries(self.staged(), &self.manifest)
    }
}

/// What the package in `dir` carries, one line each, such as `skills: a, b`:
/// the directories `skills`, `prompts`, `themes` and `tui`, its prompt file,
/// the platforms it has binaries for, and a Lua extension's raised memory
/// cap when its manifest sets `memory_mib` above 1. The directories are
/// `docs/extensions.md`, "What a package holds".
pub(crate) fn carries(dir: &Path, manifest: &Manifest) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, label) in [
        ("skills", "skills"),
        ("prompts", "prompt templates"),
        ("themes", "themes"),
        ("tui", "TUI extension"),
    ] {
        let Ok(entries) = fs::read_dir(dir.join(name)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        if !names.is_empty() {
            lines.push(format!("{label}: {}", names.join(", ")));
        }
    }
    if let Some(prompt) = &manifest.prompt {
        lines.push(format!("system prompt text: {prompt}"));
    }
    if !manifest.binaries.is_empty() {
        let platforms: Vec<&str> = manifest.binaries.keys().map(String::as_str).collect();
        lines.push(format!(
            "binaries for {}, of which only this platform's is downloaded",
            platforms.join(", ")
        ));
    }
    if manifest.process.is_none()
        && let Some(n) = manifest.memory_mib
        && n > 1
    {
        lines.push(format!("memory cap: {n} MiB"));
    }
    lines
}

/// Everything an install will put in place, fetched and checked, holding the
/// lock over `extensions/`. Dropping it installs nothing and removes what
/// was fetched.
pub struct Plan<'c> {
    root: String,
    /// Every member a batch plan updates: reachability and resolution
    /// walk from all of them, where a personal plan walks from its root
    /// alone. Empty for a personal plan.
    roots: BTreeSet<String>,
    /// In dependency mode, the unstaged root's name, version and
    /// minimums: what the pinned copy's own manifest needs and is,
    /// seeding reachability and wants without ever staging the root
    /// itself. Absent for a personal plan.
    seed: Option<Seed>,
    items: BTreeMap<String, Item>,
    fresh: BTreeSet<String>,
    damaged: Vec<Damaged>,
    scratch: Scratch,
    _lock: Option<Lock>,
    clock: &'c dyn Clock,
    /// What the commit's binary downloads report on: the sink the
    /// `extension_release` lines are written to. `None` stays silent.
    /// Set with [`Plan::with_diagnostics`]; planning itself downloads
    /// nothing.
    debug: Option<std::sync::Arc<dyn contract::diag::DebugLog>>,
}

impl Plan<'_> {
    /// Carries the commit's binary downloads on `debug`: the sink the
    /// `extension_release` lines are written to. Planning itself
    /// downloads nothing, so this changes no plan.
    pub fn with_diagnostics(
        mut self,
        debug: Option<std::sync::Arc<dyn contract::diag::DebugLog>>,
    ) -> Self {
        self.debug = debug;
        self
    }

    /// The extensions to install, the one asked for and its dependencies.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.values()
    }

    /// Names of final-plan extensions not already installed, in name order.
    pub fn fresh_names(&self) -> impl Iterator<Item = &str> {
        self.fresh.iter().map(String::as_str)
    }

    /// The damaged directories the plan skipped: their dependency
    /// minimums are unknown, so the versions chosen did not count them.
    pub fn damaged(&self) -> &[Damaged] {
        &self.damaged
    }

    /// Puts every extension in place, then runs each install step and
    /// downloads each binary at its final path, or puts nothing in place.
    /// Returns their names, the one asked for
    /// first.
    pub fn commit(mut self) -> Result<Vec<String>, Error> {
        let observe = Observe {
            debug: self.debug.clone(),
            clock: self.clock,
        };
        self.commit_run(move |manifest, target, clock| prepare(target, manifest, clock, &observe))
    }

    /// Puts every extension in place like [`Plan::commit`], but every
    /// member was prepared while staging: each install step runs at its
    /// final path, and no binary is downloaded. The lock stays held: the
    /// caller releases it with [`Plan::unlock`] before the docs swap.
    pub(crate) fn commit_prepared(&mut self) -> Result<Vec<String>, Error> {
        self.commit_run(|manifest, target, clock| {
            crate::prepare::install_step(target, manifest, clock)
        })
    }

    /// Releases the lock over `extensions/`, after the commit and before
    /// the docs swap.
    pub(crate) fn unlock(&mut self) {
        self._lock = None;
    }

    /// Puts every staged extension in place, running `run` for each
    /// manifest at its final path, or puts nothing in place. Returns
    /// their names, the plan's root first.
    fn commit_run(
        &mut self,
        run: impl Fn(&Manifest, &Path, &dyn Clock) -> Result<(), Error>,
    ) -> Result<Vec<String>, Error> {
        let paths: Vec<Paths> = self.items.values().map(|i| i.paths.clone()).collect();
        let manifests: Vec<&Manifest> = self.items.values().map(|i| &i.manifest).collect();
        let clock = self.clock;
        commit_all(
            &paths,
            |from, to| fs::rename(from, to),
            |i, target| {
                // `commit_all` only calls `run` with an index into `staged`,
                // which this vector parallels.
                let Some(manifest) = manifests.get(i) else {
                    return Err(io(target)(std::io::Error::other(
                        "a commit step without a staged manifest",
                    )));
                };
                run(manifest, target, clock)
            },
        )?;
        let mut names: Vec<String> = self.items.keys().cloned().collect();
        names.sort_by_key(|n| *n != self.root);
        Ok(names)
    }

    /// Stages one archive-covered member over what the batch plan staged
    /// from git: removes the fetched copy, then stages the unpacked
    /// archive member `adopted` with the release's version and commit,
    /// keeping the installed requested and floor. The item carries no
    /// changes: the archive is the update.
    pub(crate) fn adopt(
        &mut self,
        home: &Path,
        fiber_version: &str,
        adopted: &Adopted<'_>,
    ) -> Result<(), Error> {
        let Adopted {
            installed,
            version,
            commit,
            source,
        } = adopted;
        let record = Record {
            name: installed.name.clone(),
            provenance: Provenance::Git {
                commit: (*commit).into(),
            },
            version: (*version).into(),
            requested: installed.requested,
            floor: installed.floor.clone(),
        };
        let (manifest, providers, paths) = stage(home, source, fiber_version, &record)?;
        if manifest.name != installed.name {
            remove(&paths.fresh)?;
            return Err(Error::WrongName {
                asked: installed.name.clone(),
                found: manifest.name,
            });
        }
        retire(self, &installed.name)?;
        self.items.insert(
            installed.name.clone(),
            Item {
                name: installed.name.clone(),
                version: record.version,
                provenance: record.provenance,
                changes: None,
                manifest,
                providers,
                paths,
            },
        );
        Ok(())
    }
}

impl Drop for Plan<'_> {
    fn drop(&mut self) {
        for item in self.items.values() {
            remove(&item.paths.fresh).unwrap_or(());
        }
    }
}

/// The unstaged root a dependency plan resolves from: its name, its
/// manifest's version, and the minimums it states. Back-edge minima on
/// the root's name are validated against the version with `root_meets`.
struct Seed {
    name: String,
    version: String,
    depends: BTreeMap<String, String>,
}

/// Removes the staging copy a replaced item left behind, before its
/// replacement is staged under its own name. The previous item stays in
/// the plan, so a failure here or in the later stage leaves its cleanup
/// to `Plan::drop`.
fn retire(plan: &mut Plan, name: &str) -> Result<(), Error> {
    if let Some(previous) = plan.items.get(name) {
        remove(&previous.paths.fresh)?;
    }
    Ok(())
}

/// The locked, empty plan over `home`'s extensions, with a fresh scratch
/// directory for fetching, keeping `clock` for its commit.
fn empty<'c>(home: &Path, clock: &'c dyn Clock) -> Result<Plan<'c>, Error> {
    let lock = lock(home, clock)?;
    let scratch = Scratch::create(&std::env::temp_dir(), "fiber-fetch", 0o777)?;
    Ok(Plan {
        root: String::new(),
        roots: BTreeSet::new(),
        seed: None,
        items: BTreeMap::new(),
        fresh: BTreeSet::new(),
        damaged: Vec::new(),
        scratch,
        _lock: Some(lock),
        clock,
        debug: None,
    })
}

/// Fetches and checks what the manifest `root`'s dependencies need, against
/// an empty installed set in the copy-local home `home` (`docs/extensions.md`,
/// "Pinning"): the lowest tags meeting every minimum, committed there
/// before the copy's own install step runs. The root itself is never staged
/// or committed: only its dependencies appear in the plan.
pub(crate) fn plan_depends<'c>(
    home: &Path,
    root: &Manifest,
    fiber_version: &str,
    origin: &Origin,
    clock: &'c dyn Clock,
) -> Result<Plan<'c>, Error> {
    let mut plan = empty(home, clock)?;
    plan.root = root.name.clone();
    plan.seed = Some(Seed {
        name: root.name.clone(),
        version: root.version.clone(),
        depends: root.depends.clone(),
    });
    let mut ctx = Ctx {
        home,
        fiber_version,
        origin,
        installed: &[],
        update_all: false,
        clock,
    };
    ctx.resolve(&mut plan)?;
    // Nothing is installed in the copy-local home, so all of it is fresh.
    plan.fresh = plan.items.keys().cloned().collect();
    Ok(plan)
}

/// Fetches and checks what `request` and its dependencies need. The plan
/// keeps `clock` for its commit, which runs each install step under it.
pub fn plan<'c>(
    home: &Path,
    request: &Request,
    fiber_version: &str,
    origin: &Origin,
    clock: &'c dyn Clock,
) -> Result<Plan<'c>, Error> {
    let mut plan = empty(home, clock)?;
    let listing = read(home)?;
    plan.damaged = listing.damaged;
    let mut ctx = Ctx {
        home,
        fiber_version,
        origin,
        installed: &listing.installed,
        update_all: matches!(request, Request::UpdateAll(_)),
        clock,
    };
    plan.root = match request {
        Request::Path(path) => ctx.add_path(&mut plan, path, None)?,
        Request::Install(typed) => {
            let name = full_name(typed);
            ctx.add_git(&mut plan, &name, None, true, None, false)?;
            name
        }
        Request::Update(typed) => {
            let name = full_name(typed);
            if let Some(hit) = plan.damaged.iter().find(|d| is_damaged(d, &name)) {
                return Err(Error::Damaged(hit.clone()));
            }
            let Some(have) = listing.installed.iter().find(|i| i.name == name) else {
                return Err(Error::NotInstalled { name });
            };
            match &have.provenance {
                Provenance::Path(path) => ctx.add_path(&mut plan, path, Some(&name))?,
                Provenance::Git { .. } => {
                    ctx.add_git(&mut plan, &name, None, true, None, false)?;
                    name
                }
            }
        }
        Request::UpdateAll(typed) => {
            let name = full_name(typed);
            if let Some(hit) = plan.damaged.iter().find(|d| is_damaged(d, &name)) {
                return Err(Error::Damaged(hit.clone()));
            }
            let Some(have) = listing.installed.iter().find(|i| i.name == name) else {
                return Err(Error::NotInstalled { name });
            };
            match &have.provenance {
                Provenance::Path(path) => ctx.add_path(&mut plan, path, Some(&name))?,
                Provenance::Git { .. } => {
                    ctx.stage_git_member(&mut plan, have)?;
                    name
                }
            }
        }
        Request::Reinstall(typed) => {
            let name = full_name(typed);
            // A damaged directory is taken out of the plan's damaged
            // list, so the reinstall claims its slug and no skip line is
            // printed for it; other damaged directories still get theirs.
            if plan
                .damaged
                .iter()
                .position(|hit| is_damaged(hit, &name))
                .map(|i| plan.damaged.remove(i))
                .is_some()
            {
                ctx.add_git(&mut plan, &name, None, true, None, false)?;
                name
            } else {
                let Some(have) = listing.installed.iter().find(|i| i.name == name) else {
                    return Err(Error::NotInstalled { name });
                };
                match &have.provenance {
                    Provenance::Path(path) => ctx.add_path(&mut plan, path, Some(&name))?,
                    Provenance::Git { commit } => {
                        let version = have.version.clone();
                        let asked = have.requested;
                        let commit = commit.clone();
                        ctx.add_git(&mut plan, &name, Some(version), asked, Some(&commit), false)?;
                        name
                    }
                }
            }
        }
    };
    ctx.resolve(&mut plan)?;
    let installed: BTreeSet<&str> = listing.installed.iter().map(|i| i.name.as_str()).collect();
    plan.fresh = plan
        .items
        .keys()
        .filter(|name| !installed.contains(name.as_str()))
        .cloned()
        .collect();
    Ok(plan)
}

/// Fetches and checks what every installed extension updates to, staging
/// the whole home at once as `fiber update` prepares all members
/// together (`docs/extensions.md`, "Updating"): one lock hold, each
/// member staged as an update-all run stages it alone, the same
/// resolution settling every member, one commit putting them in place.
/// Members the release's archives cover are staged from git too; the
/// update step replaces those copies from the archives afterwards.
/// Damaged members stay installed, recorded in the plan for the caller
/// to name.
pub(crate) fn plan_updates<'c>(
    home: &Path,
    fiber_version: &str,
    origin: &Origin,
    clock: &'c dyn Clock,
) -> Result<Plan<'c>, Error> {
    let mut plan = empty(home, clock)?;
    let listing = read(home)?;
    plan.damaged = listing.damaged.clone();
    // The installed set read under the same hold: every member stages
    // from this snapshot, and nothing else changes it meanwhile.
    let installed = listing.installed;
    let mut ctx = Ctx {
        home,
        fiber_version,
        origin,
        installed: &installed,
        update_all: true,
        clock,
    };
    for member in &installed {
        if plan.damaged.iter().any(|hit| is_damaged(hit, &member.name)) {
            continue;
        }
        match &member.provenance {
            Provenance::Path(path) => {
                ctx.add_path(&mut plan, path, Some(&member.name))?;
            }
            Provenance::Git { .. } => {
                ctx.stage_git_member(&mut plan, member)?;
            }
        }
        plan.roots.insert(member.name.clone());
    }
    ctx.resolve(&mut plan)?;
    let installed: BTreeSet<&str> = installed.iter().map(|i| i.name.as_str()).collect();
    plan.fresh = plan
        .items
        .keys()
        .filter(|name| !installed.contains(name.as_str()))
        .cloned()
        .collect();
    Ok(plan)
}

/// One archive-covered member `fiber update` stages over the batch plan's
/// git-staged copy: the installed entry plus the release's version and
/// commit and the member's unpacked archive directory.
pub(crate) struct Adopted<'a> {
    /// The installed member the archive replaces.
    pub installed: &'a Installed,
    /// The release's version.
    pub version: &'a str,
    /// The release tag's commit.
    pub commit: &'a str,
    /// The member's unpacked archive directory.
    pub source: &'a Path,
}

struct Ctx<'a, 'c> {
    home: &'a Path,
    fiber_version: &'a str,
    origin: &'a Origin,
    installed: &'a [Installed],
    /// Whether the plan's roots are no-name update members: their floors
    /// then still constrain resolution.
    update_all: bool,
    clock: &'c dyn Clock,
}

impl Ctx<'_, '_> {
    fn requested(&self, name: &str, asked: bool) -> bool {
        asked || self.installed.iter().any(|i| i.name == name && i.requested)
    }

    /// Refuses a name whose directory another name already has, in Fiber
    /// home or in this plan. A directory slug a damaged extension holds
    /// fails as [`Error::Damaged`]: reinstall it.
    fn slug_free(&self, plan: &Plan, name: &str) -> Result<(), Error> {
        let mine = slug(name)?;
        if let Some(hit) = plan.damaged.iter().find(|hit| is_damaged(hit, name)) {
            return Err(Error::Damaged(hit.clone()));
        }
        let others = self
            .installed
            .iter()
            .map(|i| &i.name)
            .chain(plan.items.keys());
        for other in others {
            if other != name && slug(other)? == mine {
                return Err(Error::SlugTaken {
                    name: name.into(),
                    other: other.clone(),
                });
            }
        }
        Ok(())
    }

    /// Stages the extension in a local directory. `expect` is the installed
    /// name an update must find there.
    fn add_path(
        &mut self,
        plan: &mut Plan,
        path: &Path,
        expect: Option<&str>,
    ) -> Result<String, Error> {
        let path = fs::canonicalize(path).map_err(io(path))?;
        let manifest = config::read_manifest(&path)?;
        if let Some(expect) = expect
            && manifest.name != expect
        {
            return Err(Error::WrongName {
                asked: expect.into(),
                found: manifest.name,
            });
        }
        self.slug_free(plan, &manifest.name)?;
        // A path keeps its path, and keeps any installed floor with its
        // record: the manifest names the version, not a resolution.
        let floor = self
            .installed
            .iter()
            .find(|i| i.name == manifest.name)
            .and_then(|i| i.floor.clone());
        let record = Record {
            name: manifest.name.clone(),
            provenance: Provenance::Path(path.clone()),
            version: manifest.version.clone(),
            requested: true,
            floor,
        };
        retire(plan, &manifest.name)?;
        let (manifest, providers, paths) = stage(self.home, &path, self.fiber_version, &record)?;
        let name = manifest.name.clone();
        plan.items.insert(
            name.clone(),
            Item {
                name: name.clone(),
                version: record.version,
                provenance: record.provenance,
                changes: None,
                manifest,
                providers,
                paths,
            },
        );
        Ok(name)
    }

    /// Stages the installed git member the update moves to: every installed
    /// dependent's minimum, the person's own version and the persistent
    /// floor constrain the tag before any fetch; no constraints mean the
    /// newest tag. Shared by an update-all run and the batch plan, which
    /// stages every installed git member this way.
    fn stage_git_member(&mut self, plan: &mut Plan, member: &Installed) -> Result<(), Error> {
        let mut minima = BTreeMap::new();
        for other in self.installed {
            if let Some(min) = other.depends.get(&member.name) {
                minima.insert(other.name.clone(), min.clone());
            }
        }
        if member.requested {
            minima.insert("your install".into(), member.version.clone());
        }
        if let Some(floor) = &member.floor {
            minima.insert("your update".into(), floor.clone());
        }
        let (repo, _) = split(&member.name)?;
        let tag = newest_within(&member.name, &minima, &self.origin.tags(repo, self.clock)?)?;
        self.add_git(plan, &member.name, Some(tag), false, None, true)
    }

    /// Fetches `name` at `tag`, or at its newest tag when none, and stages it.
    /// With `pin`, no tag is listed: the recorded version is fetched at
    /// the recorded commit, with no change list. With `update_all`, this
    /// staging is the no-name update's member for `name`: its tag is
    /// recorded as the floor and its diff is shown, without making it a
    /// person request.
    fn add_git(
        &mut self,
        plan: &mut Plan,
        name: &str,
        tag: Option<String>,
        asked: bool,
        pin: Option<&str>,
        update_all: bool,
    ) -> Result<(), Error> {
        let (repo, dir) = split(name)?;
        self.slug_free(plan, name)?;
        let tag = match tag {
            Some(tag) => tag,
            None => newest(&self.origin.tags(repo, self.clock)?)
                .ok_or_else(|| Error::NoTag { name: name.into() })?,
        };
        let clone = plan.scratch.path().join(slug(name)?);
        remove(&clone)?;
        let (commit, changes) = match pin {
            Some(commit) => (
                self.origin.clone_at(repo, commit, &clone, self.clock)?,
                None,
            ),
            None => {
                let old = self
                    .installed
                    .iter()
                    .find(|i| i.name == name)
                    .and_then(|i| match &i.provenance {
                        Provenance::Git { commit } => Some(commit.as_str()),
                        Provenance::Path(_) => None,
                    });
                let history = (asked || update_all) && old.is_some();
                let commit = self.origin.clone(repo, &tag, history, &clone, self.clock)?;
                let changes = old
                    .filter(|_| history)
                    .map(|old| self.origin.changes(&clone, old, dir, self.clock))
                    .transpose()?;
                (commit, changes)
            }
        };
        remove(&clone.join(".git"))?;
        // The update-all root records its tag as the floor. A person
        // naming the extension replaces every minimum, floor included;
        // every other staging keeps the installed floor, never lowering
        // it.
        let floor = if update_all {
            Some(tag.clone())
        } else if asked && pin.is_none() {
            None
        } else {
            self.installed
                .iter()
                .find(|i| i.name == name)
                .and_then(|i| i.floor.clone())
        };
        let record = Record {
            name: name.into(),
            provenance: Provenance::Git { commit },
            version: tag,
            requested: self.requested(name, asked),
            floor,
        };
        retire(plan, name)?;
        let (manifest, providers, paths) =
            stage(self.home, &clone.join(dir), self.fiber_version, &record)?;
        if manifest.name != name {
            remove(&paths.fresh)?;
            return Err(Error::WrongName {
                asked: name.into(),
                found: manifest.name,
            });
        }
        plan.items.insert(
            name.into(),
            Item {
                name: name.into(),
                version: record.version,
                provenance: record.provenance,
                changes,
                manifest,
                providers,
                paths,
            },
        );
        Ok(())
    }

    /// Settles versions at a fixpoint. A round prunes what is no longer
    /// reachable, replaces one superseded manifest, or fetches one missing
    /// package. A conflict or a failed fetch is reported only when a round
    /// changes nothing and that package is still reachable.
    fn resolve(&mut self, plan: &mut Plan) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        'round: loop {
            self.prune(plan)?;
            let state: Vec<(String, String)> = plan
                .items
                .iter()
                .map(|(n, i)| (n.clone(), i.version.clone()))
                .collect();
            if !seen.insert(state) {
                return Err(Error::Unresolved);
            }
            let wants = self.wants(plan);
            let names: Vec<String> = reachable(plan)
                .into_iter()
                .filter(|name| name != &plan.root)
                .collect();
            let mut deferred: BTreeMap<String, Error> = BTreeMap::new();
            for dep in &names {
                let Some(have) = self.chosen(plan, dep).map(str::to_owned) else {
                    continue;
                };
                let w = wants.get(dep).cloned().unwrap_or_default();
                match meets(dep, &have, &w) {
                    Ok(true) => {}
                    Ok(false) => match self.stage_dep(plan, dep, &w) {
                        Ok(()) => continue 'round,
                        Err(e) => {
                            deferred.insert(dep.clone(), e);
                        }
                    },
                    Err(e) => {
                        deferred.insert(dep.clone(), e);
                    }
                }
            }
            for dep in &names {
                if self.chosen(plan, dep).is_some() {
                    continue;
                }
                let w = wants.get(dep).cloned().unwrap_or_default();
                match self.stage_dep(plan, dep, &w) {
                    Ok(()) => continue 'round,
                    Err(e) => {
                        deferred.insert(dep.clone(), e);
                    }
                }
            }
            if let Some(err) = deferred.into_values().next() {
                return Err(err);
            }
            // A personal plan replaces its root's minimum with the new
            // version; a dependency plan validates back-edge minima on
            // the root's name against the unstaged root manifest's
            // version, and the root stays unstaged.
            if let Some(w) = wants.get(&plan.root) {
                if let Some(seed) = &plan.seed {
                    root_meets(&plan.root, &seed.version, w)?;
                } else {
                    let have = plan
                        .items
                        .get(&plan.root)
                        .map(|item| item.version.clone())
                        .unwrap_or_default();
                    root_meets(&plan.root, &have, w)?;
                }
            }
            return Ok(());
        }
    }

    /// The version staged for `dep`, or the installed one when this plan
    /// leaves it alone.
    fn chosen<'a>(&'a self, plan: &'a Plan, dep: &str) -> Option<&'a str> {
        plan.items
            .get(dep)
            .map(|item| item.version.as_str())
            .or_else(|| {
                self.installed
                    .iter()
                    .find(|installed| installed.name == dep)
                    .map(|installed| installed.version.as_str())
            })
    }

    /// Drops staged packages nothing reachable depends on any more, and the
    /// directories fetched for them.
    fn prune(&self, plan: &mut Plan) -> Result<(), Error> {
        let reach = reachable(plan);
        let dropped: Vec<String> = plan
            .items
            .keys()
            .filter(|name| !reach.contains(*name))
            .cloned()
            .collect();
        for name in dropped {
            if let Some(item) = plan.items.remove(&name) {
                remove(&item.paths.fresh)?;
            }
        }
        Ok(())
    }

    /// Stages the lowest tag of `dep` that meets `wants`. A slug a
    /// damaged directory holds fails before any fetch: reinstall it.
    fn stage_dep(
        &mut self,
        plan: &mut Plan,
        dep: &str,
        wants: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        if let Some(hit) = plan.damaged.iter().find(|hit| is_damaged(hit, dep)) {
            return Err(Error::Damaged(hit.clone()));
        }
        let (repo, _) = split(dep)?;
        let tag = pick(dep, wants, &self.origin.tags(repo, self.clock)?)?;
        self.add_git(plan, dep, Some(tag), false, None, false)
    }

    /// Each reachable extension's dependencies and, for an installed one the
    /// plan leaves alone, the dependencies it has: name to requirer to
    /// minimum. A version a person installed or updated by name stays a
    /// minimum for that extension, under `your install`, until its record
    /// is gone (`docs/extensions.md`, "Versions"). The plan's root is left
    /// out: this install or update replaces that minimum with the new version.
    fn wants(&self, plan: &Plan) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut wants: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut add = |name: &str, depends: &BTreeMap<String, String>| {
            for (dep, min) in depends {
                wants
                    .entry(dep.clone())
                    .or_default()
                    .insert(name.to_owned(), min.clone());
            }
        };
        for item in plan.items.values() {
            add(&item.name, &item.manifest.depends);
        }
        for have in self.installed {
            if !plan.items.contains_key(&have.name) {
                add(&have.name, &have.depends);
            }
        }
        for have in self.installed {
            if have.requested && have.name != plan.root {
                wants
                    .entry(have.name.clone())
                    .or_default()
                    .insert("your install".into(), have.version.clone());
            }
        }
        // A floor an update-everything run left keeps its version as a
        // minimum, even for the plan's root: replacing the root's person
        // minimum never drops its floor. Only a person replacing the root
        // by name starts its minima over.
        for have in self.installed {
            if have.name == plan.root && !self.update_all {
                continue;
            }
            if let Some(floor) = &have.floor {
                wants
                    .entry(have.name.clone())
                    .or_default()
                    .insert("your update".into(), floor.clone());
            }
        }
        // The unstaged root's own dependencies, under its name: the seed
        // a dependency plan resolves from, absent for a personal plan.
        if let Some(seed) = &plan.seed {
            for (dep, min) in &seed.depends {
                wants
                    .entry(dep.clone())
                    .or_default()
                    .insert(seed.name.clone(), min.clone());
            }
        }
        wants
    }
}

/// Whether `name` is the damaged directory `hit`: its directory's slug.
fn is_damaged(hit: &Damaged, name: &str) -> bool {
    slug(name).is_ok_and(|mine| mine == hit.dir)
}

/// The names reachable from the request through the staged manifests,
/// including dependencies not fetched yet. In dependency mode the unstaged
/// root's own dependencies seed the walk, so a back-edge onto the root
/// ends there instead of staging it.
fn reachable(plan: &Plan) -> BTreeSet<String> {
    let mut reach = BTreeSet::from([plan.root.clone()]);
    reach.extend(plan.roots.iter().cloned());
    let mut queue: Vec<String> = reach.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        let staged = plan.items.get(&name).map(|item| &item.manifest.depends);
        let seeded = plan
            .seed
            .as_ref()
            .filter(|seed| seed.name == name)
            .map(|seed| &seed.depends);
        if let Some(depends) = staged.or(seeded) {
            for dep in depends.keys() {
                if reach.insert(dep.clone()) {
                    queue.push(dep.clone());
                }
            }
        }
    }
    reach
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
