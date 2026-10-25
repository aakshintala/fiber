//! The repository extensions a session loads, and the notices choosing
//! them raised (`docs/extensions.md`, "Code a repository ships"). A set is
//! built from the workspace's declarations once per session start and
//! reload; the session's loaders iterate it merged with the personal
//! installs, so an approved, pinned copy loads in place of a personal
//! install of the same name. No entry's directory is ever the repository's
//! own files: every repository entry's directory is `pinned/<hash>/` or a
//! real directory under its `.fiber-depends/extensions/`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::ProjectKey;
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{Notice, OfferedKind};

use super::content::{Index, hash};
use super::declared::{RepoItem, declared_items};
use super::store::{CopyHold, Decision, Fetch, Store};
use crate::Error;
use crate::git::short_name;

/// The repository extensions a session loads, the notices choosing them
/// raised, and the holds that keep their copies for the session's life.
#[derive(Debug, Default)]
pub struct RepositorySet {
    pub(crate) entries: Vec<Candidate>,
    /// Dependencies staged while each copy pins: appended to `entries`
    /// after every package, so a declared package beats a dependency of
    /// the same name in every declaration order.
    pub(crate) staged: Vec<Candidate>,
    pub(crate) notices: Vec<Notice>,
    pub(crate) holds: Vec<CopyHold>,
    pub(crate) required: Vec<String>,
    /// Each declared hook's current outcome: approved with its pinned
    /// mapping, unapproved with its hash, or skipped. Copy failures are
    /// in `hook_failures`, classified by the repository's `required`.
    #[allow(dead_code, reason = "the session snapshot reads the hook outcomes")]
    pub(crate) hooks: BTreeMap<String, super::hooks::HookState>,
    /// Each approved hook whose copy could not be made, with the
    /// repository's `required`.
    #[allow(dead_code, reason = "the session gate reads required copy failures")]
    pub(crate) hook_failures: Vec<super::hooks::HookFailure>,
}

impl RepositorySet {
    /// The set for the session's home, project and repository workspace:
    /// each declared extension, in declaration order, whose current
    /// content the session did not skip and this project approved, held
    /// through a bounded copy try, with the dependencies installed in each
    /// copy after it. `skipped` holds the `(name, content hash)` of each
    /// extension content the session skipped. A workspace whose
    /// declarations hold no extension touches nothing in Fiber home.
    /// Failing to gather the declarations, or a copy that cannot be made,
    /// leaves that entry out with a notice; only `Busy` keeps trying.
    pub fn load(
        home: &Path,
        project: &ProjectKey,
        workspace: &Path,
        skipped: &BTreeSet<(String, String)>,
        fetch: Fetch,
    ) -> Self {
        let clock = fetch.clock();
        let mut set = Self::default();
        // A declaration that fails to read, or a workspace that no longer
        // exists, gives no entry and no notice: the offer step reads the
        // same declarations and reports them, so a second report would say
        // it twice.
        let items = match declared_items(workspace, clock.as_ref()) {
            Ok(items) => items,
            Err(_) => return set,
        };
        let declared: Vec<&RepoItem> = items
            .iter()
            .filter(|item| item.kind == OfferedKind::Extension)
            .collect();
        let has_hooks = items.iter().any(|item| item.kind == OfferedKind::Hook);
        // A workspace declaring neither extensions nor hooks touches nothing
        // in Fiber home: no `pinned.json` is loaded or saved, and no
        // `pinned/` is made.
        if declared.is_empty() && !has_hooks {
            return set;
        }
        let store = Store::new(home, project, Arc::clone(&clock)).fetching(fetch);
        // One index for every hash below, saved once: an unchanged declared
        // file is compared by size and modification time, never read
        // (`docs/performance.md`).
        let mut index = Index::load(home);
        let mut hashed = false;
        for item in declared {
            // A file that vanished between gathering and hashing is left
            // out: it is the item the offer step fails the run on.
            let current = match hash(&mut index, item) {
                Ok(current) => current,
                Err(_) => continue,
            };
            hashed = true;
            // A skip holds for the session: skipped content never starts in
            // it, even when approved meanwhile.
            if skipped.contains(&(item.name.clone(), current.clone())) {
                continue;
            }
            if !matches!(
                store.decision(OfferedKind::Extension, &current),
                Some(Decision::Approve)
            ) {
                continue;
            }
            if item.required {
                set.required.push(item.name.clone());
            }
            match store.try_ensure_copy(item, &current) {
                Ok(hold) => set.pinned(item, &store.copy_dir(&current), hold),
                Err(error) => {
                    set.notices.push(copy_notice(item, &error));
                }
            }
        }
        // The index only saves a re-hash; failing to write it costs nothing
        // else.
        if hashed {
            index.save().unwrap_or(());
        }
        // Packages first, in declaration order; then dependencies, in the
        // order their packages declared them: settling keeps the first
        // entry of a name, so a declared package beats a same-named
        // dependency in every order.
        let mut staged = std::mem::take(&mut set.staged);
        set.entries.append(&mut staged);
        set.settle();
        // Hooks run from their pinned copies, never the repository's files.
        // Each approved hook uses a blocking copy, so a pruned copy is
        // rebuilt; a copy that cannot be made is that hook's failure only.
        // Holds join the session's existing holds, keeping one collection.
        match super::hooks::repository_hooks(home, project, workspace, Arc::clone(&clock), skipped)
        {
            Ok((states, holds)) => {
                for (name, state) in states {
                    match state {
                        Ok(state) => {
                            set.hooks.insert(name, state);
                        }
                        Err(failure) => {
                            // A copy that cannot be made fails only that
                            // hook: a required one stops the start at the
                            // gate, any other is skipped with a notice.
                            if failure.required {
                                set.hook_failures.push(failure);
                            } else {
                                set.notices.push(Notice {
                                    code: ErrorCode::HookFailed,
                                    message: format!(
                                        "The repository's approved copy of `{}` could not be made, so it was not loaded: {}",
                                        failure.name, failure.error,
                                    ),
                                    extension: Some(config::full_name("hooks")),
                                });
                            }
                        }
                    }
                }
                set.holds.extend(holds);
            }
            Err(_) => {
                // Gathering hooks failed: the offer step reports it, so no
                // second report here, and approved extensions still load.
            }
        }
        set
    }

    /// The set for a one-shot command in `workspace`: each declared
    /// extension whose current content this project approved, held only
    /// when its copy is already pinned and finished. A missing or
    /// unfinished copy is skipped silently: nothing is built and nothing
    /// is written — no `pinned/` directory, no lock file, no hash index
    /// save — and no notice is raised. A workspace whose declarations
    /// fail to read, or that declares no extension, gives the empty set.
    /// Every held entry keeps its shared `.ready` lock for the loading
    /// process's life, so a concurrent prune cannot remove a copy
    /// between load and use.
    pub fn load_pinned(
        home: &Path,
        project: &ProjectKey,
        workspace: &Path,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let mut set = Self::default();
        // A declaration that fails to read gives no entry and no notice,
        // as in `load`.
        let items = match declared_items(workspace, clock.as_ref()) {
            Ok(items) => items,
            Err(_) => return set,
        };
        let declared: Vec<&RepoItem> = items
            .iter()
            .filter(|item| item.kind == OfferedKind::Extension)
            .collect();
        // A workspace declaring no extension touches nothing in Fiber home.
        if declared.is_empty() {
            return set;
        }
        let store = Store::new(home, project, clock);
        // The index is read but never saved: an unchanged declared file
        // is still compared by size and modification time, never read,
        // while the load writes nothing.
        let mut index = Index::load(home);
        for item in declared {
            // A file that vanished between gathering and hashing is left
            // out, as in `load`.
            let current = match hash(&mut index, item) {
                Ok(current) => current,
                Err(_) => continue,
            };
            if !matches!(
                store.decision(OfferedKind::Extension, &current),
                Some(Decision::Approve)
            ) {
                continue;
            }
            if item.required {
                set.required.push(item.name.clone());
            }
            let copy = store.copy_dir(&current);
            // Only an already-pinned finished copy loads: a missing copy
            // is skipped, never rebuilt, and a dependency-bearing copy
            // without its completion marker is legacy, not finished.
            let hold = match store.try_hold_finished(&current) {
                Ok(hold) => hold,
                Err(_) => continue,
            };
            // Any failure reads as unfinished: the copy is skipped
            // silently, never rebuilt.
            if !store.finished(item, &copy).unwrap_or(false) {
                continue;
            }
            set.pinned(item, &copy, hold);
        }
        // Packages first, then dependencies, settling as in `load`: the
        // first entry of a name wins in every declaration order.
        let mut staged = std::mem::take(&mut set.staged);
        set.entries.append(&mut staged);
        set.settle();
        // A one-shot command never reports the set: a skipped copy stays
        // silent, so only the providers load's shadow notices print.
        set.notices.clear();
        set
    }

    /// A copy the session holds: its entry, then its dependencies'
    /// entries, in declaration order. A manifest that does not read leaves
    /// the copy out with one notice; dependencies that are not real
    /// directories leave only themselves out, while the copy still loads.
    fn pinned(&mut self, item: &RepoItem, copy: &Path, hold: CopyHold) {
        let manifest = match config::read_manifest(copy) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.notices.push(unmade(item, &error));
                return;
            }
        };
        let name = manifest.name.clone();
        let depends = !manifest.depends.is_empty();
        self.holds.push(hold);
        self.entries.push(Candidate {
            key: slug_key(&name),
            name: name.clone(),
            version: manifest.version.clone(),
            dir: copy.to_path_buf(),
            manifest,
        });
        if depends {
            self.dependencies(item, copy);
        }
    }

    /// The dependencies installed in the copy, in directory order: only
    /// real directories are read, so a link or file where the dependency
    /// home goes, or under it, leaves out this copy's dependencies with one
    /// notice, while the copy itself still loads. A damaged dependency
    /// directory gets one notice with the damage's text.
    fn dependencies(&mut self, item: &RepoItem, copy: &Path) {
        let depends = copy.join(".fiber-depends");
        let root = config::extensions_dir(&depends);
        for path in [&depends, &root] {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_dir() => {}
                // A home or root that is missing or cannot be read holds
                // no dependencies.
                Ok(_) => {
                    self.notices.push(link_notice(item, path));
                    return;
                }
                Err(_) => return,
            }
        }
        let mut paths: Vec<PathBuf> = match fs::read_dir(&root) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_default(),
            Err(_) => return,
        };
        paths.sort();
        for path in &paths {
            // The install lock Fiber itself leaves in every extensions
            // root is not a dependency.
            if path.file_name().is_some_and(|name| name == ".lock") {
                continue;
            }
            if !is_real_dir(path) {
                self.notices.push(link_notice(item, path));
                return;
            }
        }
        match crate::installed::read_entries(&depends) {
            Ok(found) => {
                for hit in &found.damaged {
                    self.notices.push(Notice {
                        code: ErrorCode::ExtensionFailed,
                        message: hit.to_string(),
                        extension: Some(short_name(&hit.name).to_owned()),
                    });
                }
                for entry in found.found {
                    let name = entry.installed.name.clone();
                    self.staged.push(Candidate {
                        key: slug_key(&name),
                        name,
                        // The record's version: the tag the dependency was
                        // fetched at, not its manifest's.
                        version: entry.installed.version,
                        dir: entry.dir,
                        manifest: entry.manifest,
                    });
                }
            }
            Err(error) => {
                self.notices.push(copy_notice(item, &error));
            }
        }
    }

    /// The first entry of a name wins: every declared package before any
    /// dependency, then declaration order, then each copy's dependencies
    /// in directory order. A declared package beats a same-named
    /// dependency in every declaration order; two declared packages keep
    /// the first declared. Each later entry of a name already in the set
    /// is dropped with one notice naming both versions, so two versions
    /// never load under one name.
    fn settle(&mut self) {
        let mut kept: BTreeMap<String, String> = BTreeMap::new();
        let mut settled = Vec::new();
        for entry in std::mem::take(&mut self.entries) {
            if let Some(version) = kept.get(&entry.name) {
                self.notices.push(Notice {
                    code: ErrorCode::ExtensionShadowed,
                    message: format!(
                        "The repository's `{}` {version} loads in this project; `{}` {} \
                         does not: two versions never load under one name.",
                        short_name(&entry.name),
                        short_name(&entry.name),
                        entry.version,
                    ),
                    extension: Some(entry.name.clone()),
                });
                continue;
            }
            kept.insert(entry.name.clone(), entry.version.clone());
            settled.push(entry);
        }
        self.entries = settled;
    }
}

/// One extension a loader starts: a personal install or a repository copy.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) dir: PathBuf,
    pub(crate) manifest: config::Manifest,
    /// The sort key: a personal entry's directory name, a repository
    /// entry's slug.
    pub(crate) key: String,
}

/// A repository entry's sort key: its slug, or its name when it has none.
fn slug_key(name: &str) -> String {
    crate::install::slug(name).unwrap_or_else(|_| name.to_owned())
}

/// Whether `path` is a real directory: symbolic links and files fail
/// without being followed.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_dir())
        .unwrap_or(false)
}

/// The notice a copy that cannot be made leaves: `Busy` gets its own
/// sentence, since [`Error::Busy`]'s text names `fiber extension install`.
fn copy_notice(item: &RepoItem, error: &Error) -> Notice {
    if matches!(error, Error::Busy) {
        return Notice {
            code: ErrorCode::ExtensionFailed,
            message: format!(
                "The repository's approved copy of `{}` was being changed by another Fiber, \
                 so it was not loaded.",
                short_name(&item.name),
            ),
            extension: Some(item.name.clone()),
        };
    }
    unmade(item, error)
}

/// The notice a copy that cannot be read or built leaves, with the
/// failure's text.
fn unmade(item: &RepoItem, why: impl std::fmt::Display) -> Notice {
    Notice {
        code: ErrorCode::ExtensionFailed,
        message: format!(
            "The repository's approved copy of `{}` could not be made, so it was not loaded: {why}",
            short_name(&item.name),
        ),
        extension: Some(item.name.clone()),
    }
}

/// The notice a dependency home that is not real directories leaves: the
/// copy itself still loads, its dependencies do not.
fn link_notice(item: &RepoItem, path: &Path) -> Notice {
    Notice {
        code: ErrorCode::ExtensionFailed,
        message: format!(
            "The repository's approved copy of `{}` has a link or a file at {}, \
             so its dependencies were not loaded.",
            short_name(&item.name),
            path.display(),
        ),
        extension: Some(item.name.clone()),
    }
}

/// The personal installs left after shadowing and `enabled`, merged with
/// the set's entries, in slug order; one `extension_shadowed` notice per
/// shadowed personal install, enabled or not. A name appears at most once:
/// the set already settled its own duplicates.
pub(crate) fn candidates(
    personal: Vec<crate::installed::Entry>,
    set: &RepositorySet,
    enabled: &dyn Fn(&str) -> bool,
) -> (Vec<Candidate>, Vec<Notice>) {
    let mut notices = Vec::new();
    let mut merged = Vec::new();
    for entry in personal {
        let name = entry.installed.name.clone();
        // An approved repository copy loads in place of the personal
        // install of the same name, whether or not it is enabled: the
        // personal install loads in every other project.
        if let Some(kept) = set.entries.iter().find(|found| found.name == name) {
            notices.push(Notice {
                code: ErrorCode::ExtensionShadowed,
                message: format!(
                    "The repository's approved copy of `{}` {} loads in this project \
                     in place of your install, {}.",
                    short_name(&name),
                    kept.version,
                    entry.installed.version,
                ),
                extension: Some(name),
            });
            continue;
        }
        if !enabled(&name) {
            continue;
        }
        let key = entry
            .dir
            .file_name()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        merged.push(Candidate {
            name,
            version: entry.installed.version,
            dir: entry.dir,
            manifest: entry.manifest,
            key,
        });
    }
    merged.extend(set.entries.iter().map(|found| Candidate {
        name: found.name.clone(),
        version: found.version.clone(),
        dir: found.dir.clone(),
        manifest: found.manifest.clone(),
        key: found.key.clone(),
    }));
    merged.sort_by(|a, b| a.key.cmp(&b.key));
    (merged, notices)
}

/// One approved repository extension, as `fiber extension list` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedExtension {
    /// The package manifest's name.
    pub name: String,
    /// The package manifest's version.
    pub version: String,
    /// The declared path, as the repository declares it.
    pub path: String,
    /// The full content hash the project approved.
    pub hash: String,
}

/// Every declared package whose current content the project approved, in
/// declaration order: never'd, unapproved and skipped content is not
/// listed, nor are dependencies inside a copy. Listing makes no copy and
/// takes no lock under `pinned/`.
pub fn approved_extensions(
    home: &Path,
    project: &ProjectKey,
    workspace: &Path,
    clock: &dyn Clock,
) -> Result<Vec<ApprovedExtension>, Error> {
    let items = declared_items(workspace, clock)?;
    let mut index = Index::load(home);
    let mut hashed = false;
    let mut approved = Vec::new();
    for item in items
        .iter()
        .filter(|item| item.kind == OfferedKind::Extension)
    {
        let current = hash(&mut index, item)?;
        hashed = true;
        if !matches!(
            Store::decision_in(home, project, OfferedKind::Extension, &current),
            Some(Decision::Approve)
        ) {
            continue;
        }
        let Ok(manifest) = config::read_manifest(&item.dir) else {
            continue;
        };
        approved.push(ApprovedExtension {
            name: manifest.name,
            version: manifest.version,
            path: item.path.clone(),
            hash: current,
        });
    }
    if hashed {
        index.save().unwrap_or(());
    }
    Ok(approved)
}

/// The declared extension `typed` names whose current content the project
/// approved: `typed` is the short or the full name. Nothing approved, or
/// approved for other content only, is `None`.
pub fn approved_extension(
    home: &Path,
    project: &ProjectKey,
    workspace: &Path,
    typed: &str,
    clock: &dyn Clock,
) -> Result<Option<(RepoItem, String)>, Error> {
    let wanted = config::full_name(typed);
    let items = declared_items(workspace, clock)?;
    let mut index = Index::load(home);
    let mut hashed = false;
    for item in items
        .iter()
        .filter(|item| item.kind == OfferedKind::Extension)
    {
        if item.name != wanted {
            continue;
        }
        let current = hash(&mut index, item)?;
        hashed = true;
        if matches!(
            Store::decision_in(home, project, OfferedKind::Extension, &current),
            Some(Decision::Approve)
        ) {
            // `hashed` is true on every path that reaches here.
            index.save().unwrap_or(());
            return Ok(Some((item.clone(), current)));
        }
    }
    if hashed {
        index.save().unwrap_or(());
    }
    Ok(None)
}
