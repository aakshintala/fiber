//! Single-use pairing codes and the live identities revoked with device records.
//! Implements `docs/invocation.md`, "Remote clients", and `docs/state.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contract::{
    ErrorCode,
    clock::{Wake, wall_ms},
};
use log::diag::Severity;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Map, Value, json};

use crate::connection::{Hub, lock};
use crate::devices;

type Refusal = (ErrorCode, String);
type Fill = Box<dyn FnMut(&mut [u8]) -> Result<(), ()> + Send>;
pub(crate) const CODE_LIFETIME: Duration = Duration::from_secs(600);
const CODE_LEN: usize = 10;
const CODE_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const NAME_MAX: usize = 64;
const NAME_RULE: &str = "A device name is 1 to 64 letters, digits, dots, hyphens or underscores, does not start with a dot, and is not `local`.";

pub(crate) struct DeviceName(String);
impl DeviceName {
    pub(crate) fn parse(name: &str) -> Result<Self, Refusal> {
        if name.is_empty()
            || name.len() > NAME_MAX
            || name.starts_with('.')
            || name == "local"
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        {
            return Err((ErrorCode::InvalidArguments, NAME_RULE.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }
}

pub(crate) struct DeviceToken(pub(crate) String);
struct PairingCode(String);
impl fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceToken(..)")
    }
}
impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(..)")
    }
}
fn code_from(bytes: [u8; CODE_LEN]) -> PairingCode {
    PairingCode(
        bytes
            .iter()
            .filter_map(|b| CODE_ALPHABET.get(usize::from(b & 31)))
            .map(|b| char::from(*b))
            .collect(),
    )
}
fn token_from(bytes: [u8; 32]) -> DeviceToken {
    DeviceToken(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) enum Who {
    Local,
    Device(Arc<Presented>),
}
impl Who {
    pub(crate) fn by(&self) -> String {
        match self {
            Self::Local => "local".to_owned(),
            Self::Device(p) => p.record.clone(),
        }
    }
    pub(crate) fn revoked(&self) -> bool {
        match self {
            Self::Local => false,
            Self::Device(p) => p.revoked.load(Ordering::SeqCst),
        }
    }
}
pub(crate) struct Presented {
    n: u64,
    record: String,
    fingerprint: String,
    revoked: AtomicBool,
    owed: Mutex<Option<Notice>>,
}
struct Notice {
    line: crate::feed::Line,
    device: String,
    by: String,
}
struct Pending {
    device: DeviceName,
    issuer: String,
    minted: Instant,
}
#[derive(Clone)]
struct Live {
    presented: Arc<Presented>,
    writer: Option<Arc<Mutex<UnixStream>>>,
    shutdown: Option<Arc<UnixStream>>,
    web: Option<Arc<crate::port::Web>>,
}
impl Live {
    fn shut(&self) {
        if let Some(stream) = &self.shutdown {
            stream.shutdown(Shutdown::Both).unwrap_or(());
        }
        if let Some(web) = &self.web {
            web.shut();
        }
    }
}
struct State {
    pending: BTreeMap<String, Pending>,
    live: BTreeMap<u64, Live>,
    fill: Fill,
    #[cfg(test)]
    pause: Option<(Pause, Box<dyn FnOnce() + Send>)>,
}
pub(crate) struct Pairing {
    state: Mutex<State>,
}
pub(crate) enum Outcome<T> {
    Done(T),
    Dropped,
}
pub(crate) enum Refused {
    Pairing,
    Io(String),
}
pub(crate) struct Revoked {
    live: Vec<Live>,
    notice: Notice,
    warning: bool,
    done: Arc<AtomicBool>,
    end: Instant,
}

fn name(args: &Map<String, Value>) -> Result<DeviceName, Refusal> {
    if args.len() != 1 {
        return Err((
            ErrorCode::InvalidArguments,
            "Expected one string `device`.".to_owned(),
        ));
    }
    DeviceName::parse(args.get("device").and_then(Value::as_str).ok_or_else(|| {
        (
            ErrorCode::InvalidArguments,
            "Expected one string `device`.".to_owned(),
        )
    })?)
}
fn io_failure(hub: &Hub, name: &str, error: impl fmt::Display) -> Refusal {
    (
        ErrorCode::IoFailed,
        format!(
            "{}: {error}",
            hub.home.join("hub/devices").join(name).display()
        ),
    )
}
fn notice(hub: &Hub, change: &str, device: &str, by: &str) -> Notice {
    let payload = Map::from_iter([
        ("change".to_owned(), Value::from(change)),
        ("device".to_owned(), Value::from(device)),
        ("by".to_owned(), Value::from(by)),
    ]);
    Notice {
        line: crate::feed::to_line(&contract::HubLine {
            kind: "device_changed".to_owned(),
            ts: wall_ms(hub.clock.wall()),
            schema_version: contract::SCHEMA_VERSION,
            payload,
        }),
        device: device.to_owned(),
        by: by.to_owned(),
    }
}
fn warn(hub: &Hub) {
    hub.diag.line(
        Severity::Warn,
        None,
        "io_failed",
        &format!(
            "{}: directory sync failed; a crash could undo the device record change.",
            hub.home.join("hub/devices").display()
        ),
    );
}
fn prune(state: &mut State, now: Instant) {
    state
        .pending
        .retain(|_, pending| now.saturating_duration_since(pending.minted) <= CODE_LIFETIME);
}

impl Pairing {
    pub(crate) fn new() -> Self {
        Self::build(Box::new(|bytes| {
            SystemRandom::new().fill(bytes).map_err(|_| ())
        }))
    }
    fn build(fill: Fill) -> Self {
        Self {
            state: Mutex::new(State {
                pending: BTreeMap::new(),
                live: BTreeMap::new(),
                fill,
                #[cfg(test)]
                pause: None,
            }),
        }
    }
    #[cfg(test)]
    pub(crate) fn with_random(fill: Fill) -> Self {
        Self::build(fill)
    }

    pub(crate) fn mint(
        &self,
        hub: &Hub,
        who: &Who,
        args: &Map<String, Value>,
    ) -> Result<Outcome<Value>, Refusal> {
        let mut state = lock(&self.state);
        if who.revoked() {
            return Ok(Outcome::Dropped);
        }
        let device = name(args)?;
        let now = hub.clock.now();
        prune(&mut state, now);
        if devices::exists(&hub.home, &device.0).map_err(|e| io_failure(hub, &device.0, e))? {
            return Err((
                ErrorCode::InvalidArguments,
                format!("Device {} is already paired; revoke it first.", device.0),
            ));
        }
        for _ in 0..8 {
            let mut bytes = [0; CODE_LEN];
            (state.fill)(&mut bytes).map_err(|_| {
                (
                    ErrorCode::IoFailed,
                    "The system has no random bytes.".to_owned(),
                )
            })?;
            let code = code_from(bytes);
            let hash = devices::sha256_hex(code.0.as_bytes());
            if state.pending.contains_key(&hash) {
                continue;
            }
            state
                .pending
                .retain(|_, pending| pending.device.0 != device.0);
            state.pending.insert(
                hash,
                Pending {
                    device,
                    issuer: who.by(),
                    minted: now,
                },
            );
            return Ok(Outcome::Done(json!({"code":code.0})));
        }
        Err((
            ErrorCode::IoFailed,
            "The system has no random bytes.".to_owned(),
        ))
    }

    pub(crate) fn redeem(
        &self,
        hub: &Hub,
        n: u64,
        args: &Map<String, Value>,
    ) -> Result<(DeviceToken, Who), Refused> {
        let mut state = lock(&self.state);
        prune(&mut state, hub.clock.now());
        if args.len() != 1 {
            return Err(Refused::Pairing);
        }
        let code = args
            .get("code")
            .and_then(Value::as_str)
            .ok_or(Refused::Pairing)?;
        let hash = devices::sha256_hex(code.to_ascii_lowercase().as_bytes());
        let pending = state.pending.remove(&hash).ok_or(Refused::Pairing)?;
        #[cfg(test)]
        pause(&mut state, Pause::AfterFound);
        let device = pending.device.0;
        if devices::exists(&hub.home, &device)
            .map_err(|e| Refused::Io(io_failure(hub, &device, e).1))?
        {
            return Err(Refused::Pairing);
        }
        let mut bytes = [0; 32];
        (state.fill)(&mut bytes)
            .map_err(|_| Refused::Io("The system has no random bytes.".to_owned()))?;
        let token = token_from(bytes);
        let at = wall_ms(hub.clock.wall());
        let fingerprint = devices::sha256_hex(token.0.as_bytes());
        devices::write(
            &hub.home,
            &devices::Record {
                name: device.clone(),
                paired_at: at,
                last_connected_at: at,
                token_sha256: fingerprint.clone(),
            },
        )
        .map_err(|e| Refused::Io(io_failure(hub, &device, e).1))?;
        let warning = devices::sync_dir(&hub.home).is_err();
        let owed = notice(hub, "paired", &device, &pending.issuer);
        let who = register(&mut state, n, device, fingerprint, Some(owed));
        drop(state);
        if warning {
            warn(hub);
        }
        Ok((token, who))
    }

    pub(crate) fn authenticate(&self, hub: &Hub, n: u64, token: &str) -> Option<Who> {
        let mut state = lock(&self.state);
        let fingerprint = devices::sha256_hex(token.as_bytes());
        let record = devices::holder(&hub.home, token)?;
        #[cfg(test)]
        pause(&mut state, Pause::AfterFound);
        let warning = if DeviceName::parse(&record).is_ok() {
            devices::touch(&hub.home, &record, wall_ms(hub.clock.wall())).err()
        } else {
            None
        };
        let who = register(&mut state, n, record.clone(), fingerprint, None);
        drop(state);
        if let Some(error) = warning {
            hub.diag.line(
                Severity::Warn,
                None,
                "io_failed",
                &io_failure(hub, &record, error).1,
            );
        }
        Some(who)
    }

    pub(crate) fn attach_web(&self, who: &Who, web: Arc<crate::port::Web>) {
        if let Who::Device(p) = who {
            let mut state = lock(&self.state);
            if let Some(live) = state.live.get_mut(&p.n) {
                live.web = Some(Arc::clone(&web));
            }
            let revoked = p.revoked.load(Ordering::SeqCst);
            drop(state);
            if revoked {
                web.shut();
            }
        }
    }
    pub(crate) fn attach(&self, who: &Who, writer: &Arc<Mutex<UnixStream>>) {
        let shutdown = lock(writer).try_clone().ok().map(Arc::new);
        if let Who::Device(p) = who {
            let mut state = lock(&self.state);
            if let Some(live) = state.live.get_mut(&p.n) {
                live.writer = Some(Arc::clone(writer));
                live.shutdown = shutdown.clone();
            }
            let revoked = p.revoked.load(Ordering::SeqCst);
            drop(state);
            if revoked && let Some(stream) = shutdown {
                stream.shutdown(Shutdown::Both).unwrap_or(());
            }
        }
    }
    pub(crate) fn announce_owed(&self, hub: &Hub, who: &Who) {
        if let Who::Device(p) = who {
            let owed = lock(&p.owed).take();
            if let Some(owed) = owed {
                announce(hub, owed, "paired");
            }
        }
    }
    pub(crate) fn forget(&self, hub: &Hub, who: &Who) {
        if let Who::Device(p) = who {
            lock(&self.state).live.remove(&p.n);
        }
        self.announce_owed(hub, who);
    }
    pub(crate) fn devices(
        &self,
        hub: &Hub,
        args: &Map<String, Value>,
    ) -> Result<Option<Value>, Refusal> {
        if !args.is_empty() {
            return Err((
                ErrorCode::InvalidArguments,
                "`devices` takes no arguments.".to_owned(),
            ));
        }
        let listed = devices::list(&hub.home).map_err(|e| io_failure(hub, "", e))?;
        let devices: Vec<Value> = listed
            .into_iter()
            .map(|listed| {
                let mut value = Map::from_iter([("name".to_owned(), Value::from(listed.name))]);
                if let Some(at) = listed.paired_at {
                    value.insert("paired_at".to_owned(), Value::from(at));
                }
                if let Some(at) = listed.last_connected_at {
                    value.insert("last_connected_at".to_owned(), Value::from(at));
                }
                Value::Object(value)
            })
            .collect();
        Ok(Some(json!({"devices":devices})))
    }
    pub(crate) fn revoke(
        &self,
        hub: &Hub,
        who: &Who,
        args: &Map<String, Value>,
    ) -> Result<Outcome<Revoked>, Refusal> {
        let mut state = lock(&self.state);
        if who.revoked() {
            return Ok(Outcome::Dropped);
        }
        let device = name(args)?.0;
        let not_found = || {
            (
                ErrorCode::NotFound,
                format!("No device named {device} is paired."),
            )
        };
        let entries = match std::fs::read_dir(hub.home.join("hub/devices")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_found());
            }
            Err(error) => return Err(io_failure(hub, &device, error)),
        };
        // Read every regular record before the first unlink: a surviving copy
        // would make the token usable again after a restart.
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_failure(hub, &device, e))?;
            let record = entry.file_name();
            let display = record.to_string_lossy();
            if !entry
                .file_type()
                .map_err(|e| io_failure(hub, &display, e))?
                .is_file()
            {
                continue;
            }
            let hash = devices::token_hash(&hub.home, &record)
                .map_err(|e| io_failure(hub, &display, e))?;
            records.push((record, hash));
        }
        let fingerprint = records
            .iter()
            .find(|(record, _)| record == device.as_str())
            .map(|(_, hash)| hash.clone())
            .ok_or_else(not_found)?;
        for (record, hash) in records {
            if hash == fingerprint {
                devices::remove(&hub.home, &record)
                    .map_err(|e| io_failure(hub, &record.to_string_lossy(), e))?;
            }
        }
        let warning = devices::sync_dir(&hub.home).is_err();
        let mut live = Vec::new();
        for connection in state.live.values() {
            if connection.presented.record == device
                || fingerprint == connection.presented.fingerprint
            {
                connection.presented.revoked.store(true, Ordering::SeqCst);
                live.push(connection.clone());
            }
        }
        state.pending.retain(|_, code| code.issuer != device);
        #[cfg(test)]
        pause(&mut state, Pause::AfterRevokeMarked);
        drop(state);
        let end = hub
            .clock
            .now()
            .checked_add(crate::port::STOP_CLOSE_WAIT)
            .unwrap_or_else(|| hub.clock.now());
        let done = Arc::new(AtomicBool::new(false));
        let watchdog = std::thread::Builder::new()
            .name("hub-revoke-deadline".to_owned())
            .spawn({
                let live = live.clone();
                let done = Arc::clone(&done);
                let clock = Arc::clone(&hub.clock);
                let tick = Arc::clone(&hub.tick);
                move || {
                    {
                        let mut guard = tick.hold();
                        while !done.load(Ordering::SeqCst) && clock.now() < end {
                            let Some(next) = tick.park(clock.as_ref(), guard, Some(end)) else {
                                break;
                            };
                            guard = next;
                        }
                    }
                    for connection in live {
                        connection.shut();
                    }
                }
            });
        if watchdog.is_err() {
            for connection in &live {
                connection.shut();
            }
        }
        Ok(Outcome::Done(Revoked {
            live,
            notice: notice(hub, "revoked", &device, &who.by()),
            warning,
            done,
            end,
        }))
    }
    pub(crate) fn finish(&self, hub: &Hub, revoked: Revoked) {
        let mut threads = Vec::new();
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for live in &revoked.live {
            if let Some(writer) = &live.writer {
                let writer = Arc::clone(writer);
                let line = Arc::clone(&revoked.notice.line);
                let tick = Arc::clone(&hub.tick);
                let completed = Arc::clone(&completed);
                if let Ok(thread) = std::thread::Builder::new()
                    .name("hub-revoke-out".to_owned())
                    .spawn(move || {
                        let mut writer = lock(&writer);
                        writer.write_all(&line).unwrap_or(());
                        writer.shutdown(Shutdown::Write).unwrap_or(());
                        completed.fetch_add(1, Ordering::SeqCst);
                        tick.wake();
                    })
                {
                    threads.push(thread);
                }
            }
        }
        {
            let mut guard = hub.tick.hold();
            while (completed.load(Ordering::SeqCst) != threads.len()
                || revoked
                    .live
                    .iter()
                    .any(|live| live.web.as_ref().is_some_and(|web| !web.drained())))
                && hub.clock.now() < revoked.end
            {
                let Some(next) = hub.tick.park(hub.clock.as_ref(), guard, Some(revoked.end)) else {
                    break;
                };
                guard = next;
            }
        }
        revoked.done.store(true, Ordering::SeqCst);
        hub.tick.wake();
        for live in &revoked.live {
            live.shut();
        }
        for thread in threads {
            if thread.is_finished() {
                thread.join().unwrap_or(());
            }
        }
        announce(hub, revoked.notice, "revoked");
        if revoked.warning {
            warn(hub);
        }
    }
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        lock(&self.state).live.len()
    }
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        lock(&self.state).pending.len()
    }
    #[cfg(test)]
    pub(crate) fn pause_in_lock(&self, at: Pause, hook: impl FnOnce() + Send + 'static) {
        lock(&self.state).pause = Some((at, Box::new(hook)));
    }
}
fn register(
    state: &mut State,
    n: u64,
    record: String,
    fingerprint: String,
    owed: Option<Notice>,
) -> Who {
    let presented = Arc::new(Presented {
        n,
        record,
        fingerprint,
        revoked: AtomicBool::new(false),
        owed: Mutex::new(owed),
    });
    state.live.insert(
        n,
        Live {
            presented: Arc::clone(&presented),
            writer: None,
            shutdown: None,
            web: None,
        },
    );
    Who::Device(presented)
}
fn announce(hub: &Hub, notice: Notice, change: &str) {
    hub.feed.attention.announce(notice.line);
    hub.diag.line(
        Severity::Info,
        None,
        &format!("device_{change}"),
        &format!("Device {} was {change} by {}.", notice.device, notice.by),
    );
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pause {
    AfterFound,
    AfterRevokeMarked,
}
#[cfg(test)]
fn pause(state: &mut State, at: Pause) {
    if state.pause.as_ref().is_some_and(|(point, _)| *point == at)
        && let Some((_, hook)) = state.pause.take()
    {
        hook();
    }
}
#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;
