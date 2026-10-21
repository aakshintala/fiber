//! Where the escalation read looks: every started session's directory, its
//! project key and the identity its first line records. The population is
//! [`crate::started_sessions`]' set, walked the same way, but the first line
//! is read narrowly: only `kind`, `payload.workspace` and
//! `payload.worktree.repository`, so no other key of it is kept.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::de::{self, IgnoredAny, Visitor};
use serde::{Deserialize, Deserializer};

/// A started session's place: its `projects/<key>` directory name, its
/// directory, and the identity its first line records, if any.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) project: String,
    pub(super) dir: PathBuf,
    pub(super) identity: Option<String>,
}

/// Every session under `home/projects/*/sessions/`, sorted by id: the same
/// directories [`crate::started_sessions`] lists, in its order. Each
/// directory under a `sessions` directory that `file_type` says is a
/// directory, not a link, whose first line reads as a `session_started`
/// with a payload counts; anything else gives nothing, not an error.
pub(super) fn sessions(home: &Path) -> Vec<Place> {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for project in projects.flatten() {
        found.extend(sessions_in(&project.path().join("sessions"), &project));
    }
    found.sort_by_key(place_id);
    found
}

fn place_id(place: &Place) -> String {
    place
        .dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn sessions_in(sessions: &Path, project: &std::fs::DirEntry) -> Vec<Place> {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return Vec::new();
    };
    let project = project.file_name().to_string_lossy().into_owned();
    let mut found = Vec::new();
    for entry in entries.flatten() {
        // `file_type` does not follow a link: a linked directory is
        // not a session.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let Some((workspace, repository)) = identity_of(&dir) else {
            continue;
        };
        found.push(Place {
            project: project.clone(),
            identity: crate::resolve::recorded_identity(
                workspace.as_deref(),
                repository.as_deref(),
            )
            .map(str::to_owned),
            dir,
        });
    }
    found.sort_by_key(place_id);
    found
}

/// The two identity fields of `dir`'s first line: `None` when the log
/// cannot be read or its first line does not read as a `session_started`
/// with a payload. A first line with no trailing newline still counts.
fn identity_of(dir: &Path) -> Option<(Option<String>, Option<String>)> {
    let file = crate::open_log(dir).ok()?;
    let mut first = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut first).ok()?;
    let line: First = serde_json::from_slice(&first).ok()?;
    if line.kind.as_deref() != Some("session_started") {
        return None;
    }
    let payload = line.payload?;
    Some((
        payload.workspace,
        payload.worktree.and_then(|worktree| worktree.repository),
    ))
}

/// A log's first line, naming only its kind and the two identity fields.
/// Every other key, `parent` and `forked_from` included, is skipped
/// unread. A `payload` that is present but not an object reads as both
/// fields absent; a `worktree` that is present but not an object reads as
/// only `repository` absent.
#[derive(Deserialize)]
struct First {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, deserialize_with = "payload_field")]
    payload: Option<Payload>,
}

#[derive(Default)]
struct Payload {
    workspace: Option<String>,
    worktree: Option<Worktree>,
}

#[derive(Default)]
struct Worktree {
    repository: Option<String>,
}

/// A string field read leniently: a non-string value reads as absent, so
/// a session the wide scan lists is never dropped here. Only the arms
/// serde_json's `deserialize_any` emits for JSON values exist: `visit_some`,
/// `visit_none`, `visit_string` and `visit_bytes` never fire and are omitted
/// so no mutant can hide in them.
fn lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Lenient;
    impl<'de> Visitor<'de> for Lenient {
        type Value = Option<String>;
        // exempted: the expecting text never surfaces: no arm errors for a
        // JSON value, and `place::sessions` drops a bad first line unread.
        #[cfg_attr(false, mutants::skip)]
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string or anything else, read as absent")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Option<String>, E> {
            Ok(None)
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<Option<String>, E> {
            Ok(Some(value.to_owned()))
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<String>, E> {
            Ok(None)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Option<String>, E> {
            Ok(None)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Option<String>, E> {
            Ok(None)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Option<String>, E> {
            Ok(None)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Option<String>, A::Error> {
            while let Some((_, _)) = map.next_entry::<IgnoredAny, IgnoredAny>()? {}
            Ok(None)
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Option<String>, A::Error> {
            while let Some(_) = seq.next_element::<IgnoredAny>()? {}
            Ok(None)
        }
    }
    deserializer.deserialize_any(Lenient)
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default, deserialize_with = "lenient_string")]
            workspace: Option<String>,
            #[serde(default, deserialize_with = "worktree_field")]
            worktree: Option<Worktree>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            workspace: fields.workspace,
            worktree: fields.worktree,
        })
    }
}

impl<'de> Deserialize<'de> for Worktree {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default, deserialize_with = "lenient_string")]
            repository: Option<String>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            repository: fields.repository,
        })
    }
}

/// The `payload` key: absent means no payload, so no session; present but
/// not an object means both identity fields are absent, and the session
/// still counts.
// exempted: the surviving mutants change no behaviour `place::sessions`
// can show: wrong-type and null arms already return `Some(default)`, so the
// `Some(Default::default())` replacements are the same value;
// `visit_some`, `visit_none`, `visit_string` and `visit_bytes` never fire
// through serde_json's `deserialize_any`; and the expecting text never
// surfaces, since a bad first line drops the session. Listed-vs-dropped
// stays pinned by the population tests.
#[cfg_attr(false, mutants::skip)]
fn payload_field<'de, D>(deserializer: D) -> Result<Option<Payload>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Lenient;
    impl<'de> Visitor<'de> for Lenient {
        type Value = Option<Payload>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object or anything else, read leniently")
        }
        fn visit_none<E: de::Error>(self) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_unit<E: de::Error>(self) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_some<D2: Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Option<Payload>, D2::Error> {
            payload_field(deserializer)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Option<Payload>, A::Error> {
            Payload::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_string<E: de::Error>(self, _: String) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Option<Payload>, E> {
            Ok(Some(Payload::default()))
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Option<Payload>, A::Error> {
            while let Some(_) = seq.next_element::<IgnoredAny>()? {}
            Ok(Some(Payload::default()))
        }
    }
    deserializer.deserialize_any(Lenient)
}

/// The `worktree` key: absent, null or not an object means no repository,
/// with the workspace kept; an object names it leniently.
// exempted: the surviving mutants change no behaviour `place::sessions`
// can show: `None` and `Some(empty)` read the same repository-absent
// downstream through `and_then`, so the `Some(Default::default())`
// replacements are redundant; `visit_some`, `visit_none`, `visit_string`
// and `visit_bytes` never fire through serde_json's `deserialize_any`;
// and the expecting text never surfaces, since a bad first line drops the
// session. Repository-wins and workspace-kept stay pinned by the
// population tests.
#[cfg_attr(false, mutants::skip)]
fn worktree_field<'de, D>(deserializer: D) -> Result<Option<Worktree>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Lenient;
    impl<'de> Visitor<'de> for Lenient {
        type Value = Option<Worktree>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object or anything else, read leniently")
        }
        fn visit_none<E: de::Error>(self) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_some<D2: Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Option<Worktree>, D2::Error> {
            worktree_field(deserializer)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Option<Worktree>, A::Error> {
            Worktree::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_string<E: de::Error>(self, _: String) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Option<Worktree>, E> {
            Ok(None)
        }
        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Option<Worktree>, A::Error> {
            while let Some(_) = seq.next_element::<IgnoredAny>()? {}
            Ok(None)
        }
    }
    deserializer.deserialize_any(Lenient)
}
