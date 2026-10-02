//! `fiber install`, `update`, `remove` and `list` by name
//! (`docs/extensions.md`, "Installing"): a [`Plan`] fetches and checks
//! everything first, so a failure installs nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use config::{Manifest, ProviderData};
use serde_json::Value;

use crate::Error;
use crate::git::{Origin, full_name, split};
use crate::install::{Paths, RECORD, Record, commit_all, io, remove, slug, stage};
use crate::resolve::{meets, newest, pick};

/// More rounds of choosing versions than any real set of dependencies needs.
const ROUNDS: usize = 64;

/// What is asked for.
pub enum Request {
    /// The extension in a local directory.
    Path(PathBuf),
    /// A name, or a first-party short name.
    Install(String),
    /// An installed extension, moved to its newest version.
    Update(String),
}

/// An installed extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Its name.
    pub name: String,
    /// Its version: the tag it was fetched at.
    pub version: String,
    /// The exact commit; none when installed from a local path.
    pub commit: Option<String>,
    /// A local path, or its name.
    pub source: String,
    /// Asked for, rather than pulled in as a dependency.
    pub requested: bool,
    /// The extensions it depends on, each with a minimum version.
    pub depends: BTreeMap<String, String>,
}

/// One extension an install will put in place.
pub struct Item {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// A local path, or its name.
    pub source: String,
    /// The exact commit; none for a local path.
    pub commit: Option<String>,
    /// For an update, what changed since the installed commit.
    pub changes: Option<String>,
    /// Its manifest.
    pub manifest: Manifest,
    /// The providers it registers.
    pub providers: Vec<ProviderData>,
    paths: Paths,
}

/// Everything an install will put in place, fetched and checked. Dropping it
/// installs nothing and removes what was fetched.
pub struct Plan {
    items: BTreeMap<String, Item>,
    scratch: PathBuf,
}

impl Plan {
    /// The extensions to install, the one asked for and its dependencies.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.values()
    }

    /// Puts every extension in place, or none, and returns their names.
    pub fn commit(self) -> Result<Vec<String>, Error> {
        let paths: Vec<Paths> = self.items.values().map(|i| i.paths.clone()).collect();
        commit_all(&paths, |from, to| fs::rename(from, to))?;
        Ok(self.items.keys().cloned().collect())
    }
}

impl Drop for Plan {
    fn drop(&mut self) {
        remove(&self.scratch).unwrap_or(());
        for item in self.items.values() {
            remove(&item.paths.fresh).unwrap_or(());
        }
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Fetches and checks what `request` and its dependencies need.
pub fn plan(
    home: &Path,
    request: &Request,
    fiber_version: &str,
    origin: &Origin,
) -> Result<Plan, Error> {
    let installed = list(home)?;
    let scratch = std::env::temp_dir().join(format!(
        "fiber-fetch-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    remove(&scratch)?;
    fs::create_dir_all(&scratch).map_err(io(&scratch))?;
    let mut plan = Plan {
        items: BTreeMap::new(),
        scratch,
    };
    let mut ctx = Ctx {
        home,
        fiber_version,
        origin,
        installed: &installed,
    };
    match request {
        Request::Path(path) => {
            ctx.add_path(&mut plan, path)?;
        }
        Request::Install(typed) => ctx.add_git(&mut plan, &full_name(typed), None, true)?,
        Request::Update(typed) => {
            let name = full_name(typed);
            let Some(have) = installed.iter().find(|i| i.name == name) else {
                return Err(Error::NotInstalled { name });
            };
            match &have.commit {
                None => {
                    ctx.add_path(&mut plan, Path::new(&have.source))?;
                }
                Some(_) => ctx.add_git(&mut plan, &name, None, true)?,
            }
        }
    }
    ctx.resolve(&mut plan)?;
    Ok(plan)
}

struct Ctx<'a> {
    home: &'a Path,
    fiber_version: &'a str,
    origin: &'a Origin,
    installed: &'a [Installed],
}

impl Ctx<'_> {
    fn requested(&self, name: &str, asked: bool) -> bool {
        asked || self.installed.iter().any(|i| i.name == name && i.requested)
    }

    fn add_path(&mut self, plan: &mut Plan, path: &Path) -> Result<String, Error> {
        let manifest = config::read_manifest(path)?;
        let record = Record {
            source: path.display().to_string(),
            version: manifest.version.clone(),
            commit: None,
            requested: true,
        };
        let (manifest, providers, paths) = stage(self.home, path, self.fiber_version, &record)?;
        let name = manifest.name.clone();
        plan.items.insert(
            name.clone(),
            Item {
                name: name.clone(),
                version: record.version,
                source: record.source,
                commit: None,
                changes: None,
                manifest,
                providers,
                paths,
            },
        );
        Ok(name)
    }

    /// Fetches `name` at `tag`, or at its newest tag when none, and stages it.
    fn add_git(
        &mut self,
        plan: &mut Plan,
        name: &str,
        tag: Option<String>,
        asked: bool,
    ) -> Result<(), Error> {
        let (repo, dir) = split(name)?;
        let tag = match tag {
            Some(tag) => Some(tag),
            None => newest(&self.origin.tags(repo)?),
        };
        let clone = plan.scratch.join(slug(name)?);
        remove(&clone)?;
        let old = self
            .installed
            .iter()
            .find(|i| i.name == name)
            .and_then(|i| i.commit.as_deref());
        let history = asked && old.is_some();
        let commit = self.origin.clone(repo, tag.as_deref(), history, &clone)?;
        let changes = old
            .filter(|_| history)
            .map(|old| self.origin.changes(&clone, old, dir));
        remove(&clone.join(".git"))?;
        let source = clone.join(dir);
        let record = Record {
            source: name.into(),
            version: tag.clone().unwrap_or_default(),
            commit: Some(commit),
            requested: self.requested(name, asked),
        };
        let (manifest, providers, paths) = stage(self.home, &source, self.fiber_version, &record)?;
        if manifest.name != name {
            remove(&paths.fresh)?;
            return Err(Error::WrongName {
                asked: name.into(),
                found: manifest.name,
            });
        }
        let version = tag.unwrap_or_else(|| manifest.version.clone());
        plan.items.insert(
            name.into(),
            Item {
                name: name.into(),
                version,
                source: record.source,
                commit: record.commit,
                changes,
                manifest,
                providers,
                paths,
            },
        );
        Ok(())
    }

    /// Chooses each dependency's version until every minimum is met.
    fn resolve(&mut self, plan: &mut Plan) -> Result<(), Error> {
        for _ in 0..ROUNDS {
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
            let mut changed = false;
            for (dep, w) in &wants {
                let have = plan.items.get(dep).map(|i| i.version.as_str()).or_else(|| {
                    self.installed
                        .iter()
                        .find(|i| &i.name == dep)
                        .map(|i| i.version.as_str())
                });
                if let Some(have) = have
                    && meets(dep, have, w)?
                {
                    continue;
                }
                let (repo, _) = split(dep)?;
                let tag = pick(dep, w, &self.origin.tags(repo)?)?;
                if plan.items.get(dep).is_some_and(|i| i.version == tag) {
                    continue;
                }
                self.add_git(plan, dep, Some(tag), false)?;
                changed = true;
            }
            if !changed {
                return Ok(());
            }
        }
        Err(Error::Unresolved)
    }
}

/// The installed extensions, by name. One whose manifest cannot be read is
/// skipped: loading reports it.
pub fn list(home: &Path) -> Result<Vec<Installed>, Error> {
    let root = home.join("extensions");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(&root)(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io(&root))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let dir = entry.path();
        let Ok(manifest) = config::read_manifest(&dir) else {
            continue;
        };
        let record: Value = fs::read_to_string(dir.join(RECORD))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let text = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
        found.push(Installed {
            version: text("version").unwrap_or_else(|| manifest.version.clone()),
            commit: text("commit"),
            source: text("source").unwrap_or_else(|| dir.display().to_string()),
            requested: record
                .get("requested")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            depends: manifest.depends,
            name: manifest.name,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Removes an extension, then every dependency that nothing left needs.
/// Returns what it removed, the one asked for first.
pub fn uninstall(home: &Path, typed: &str) -> Result<Vec<String>, Error> {
    let name = full_name(typed);
    let mut left = list(home)?;
    if !left.iter().any(|i| i.name == name) {
        return Err(Error::NotInstalled { name });
    }
    let mut removed = Vec::new();
    let mut next = Some(name);
    while let Some(name) = next {
        let dir = home.join("extensions").join(slug(&name)?);
        remove(&dir)?;
        left.retain(|i| i.name != name);
        removed.push(name);
        next = left
            .iter()
            .find(|i| !i.requested && !left.iter().any(|other| other.depends.contains_key(&i.name)))
            .map(|i| i.name.clone());
    }
    Ok(removed)
}
