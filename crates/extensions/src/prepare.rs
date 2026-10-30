//! What runs after the person approves and before anything is put in place:
//! this platform's binary, and the manifest's install step
//! (`docs/extensions.md`, "Versions" and "Installing").

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::Manifest;
use contract::clock::Clock;
use contract::diag::{DebugLog, HttpRequest};
use ureq::unversioned::resolver::DefaultResolver;

/// What a release, checksum or binary download reports on: the sink
/// the `extension_release` line is written to and the clock its stamps
/// read. `None` stays silent.
#[derive(Clone)]
pub struct Observe<'a> {
    /// The sink the line is written to, or `None` for silence.
    pub debug: Option<Arc<dyn DebugLog>>,
    /// The clock the line's stamps read.
    pub clock: &'a dyn Clock,
}

impl Observe<'_> {
    /// The send stamp, `None` when silent so no clock is read at info.
    pub(crate) fn stamp(&self) -> Option<std::time::Instant> {
        self.debug.as_ref().map(|_| self.clock.now())
    }

    /// Writes the download's `extension_release` line: sanitized `host`
    /// and query/fragment-free decoded `path`, `status` only when a
    /// response arrived, `attempt: 1`, the posted (always zero) and
    /// consumed byte counts, and the stamps ordered. A send that never
    /// left (`start` `None`) writes nothing.
    pub(crate) fn line(
        &self,
        url: &str,
        start: Option<std::time::Instant>,
        headers: Option<std::time::Instant>,
        status: Option<u16>,
        consumed: u64,
    ) {
        let Some(debug) = self.debug.as_ref() else {
            return;
        };
        if !debug.debugging() {
            return;
        }
        let end = self.clock.now();
        let Some(start) = start else {
            return;
        };
        let millis = |instant: std::time::Instant| {
            u64::try_from(instant.saturating_duration_since(start).as_millis()).unwrap_or(u64::MAX)
        };
        let (host, path) = sanitized_url(url);
        let mut line = HttpRequest::for_extension_release(host, path);
        line.status = status;
        line.attempt = 1;
        line.request_bytes = 0;
        line.response_bytes = consumed;
        line.headers_ms = headers.map(millis);
        line.total_ms = millis(end);
        debug.http_request(&line);
    }
}

/// The download's `host` and `path`: the authority without userinfo,
/// the path up to any query or fragment, percent-decoded. Empty
/// pieces stay `None`.
pub(crate) fn sanitized_url(url: &str) -> (Option<String>, Option<String>) {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = (!host.is_empty()).then(|| host.to_owned());
    let path = String::from_utf8(support::percent_decode(path.as_bytes())).ok();
    (host, path)
}

use crate::Error;
use crate::host::exec;
#[cfg(test)]
use support::hex;

/// The largest binary Fiber downloads.
// debt: a fixed 256 MiB cap; raise it when a real binary needs more.
const MAX_BINARY: u64 = 268_435_456;

/// How long an install step may run before it is stopped
/// (`docs/extensions.md`, "Installing").
pub(crate) const INSTALL_STEP_DEADLINE: Duration = Duration::from_secs(1800);

/// This platform's key in a manifest's `binaries`, such as `darwin-arm64`.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Downloads this platform's binary into `dir/bin/` and checks its SHA-256.
/// Another platform's binary is never fetched. The update's pre-stage runs
/// only this, before the person's approval: the manifest's install step
/// runs once after approval, at the commit. [`prepare`] runs both.
pub(crate) fn prepare_binary(
    dir: &Path,
    manifest: &Manifest,
    observe: &Observe<'_>,
) -> Result<(), Error> {
    if let Some(binary) = manifest.binaries.get(&platform()) {
        let file = binary_name(&manifest.name).ok_or_else(|| Error::Download {
            name: manifest.name.clone(),
            why: format!("`{}` names no file", manifest.name),
        })?;
        let bytes = download_observed(&manifest.name, &binary.url, observe)?;
        let got = support::sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(binary.sha256.trim()) {
            return Err(Error::BinaryChecksum {
                name: manifest.name.clone(),
                url: binary.url.clone(),
            });
        }
        let out = dir.join("bin");
        let path = out.join(file);
        let write = fs::create_dir_all(&out).and_then(|()| {
            fs::remove_file(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o755)
                .open(&path)?;
            f.write_all(&bytes)
        });
        write.map_err(crate::install::io(&path))?;
    }
    Ok(())
}

/// Downloads this platform's binary into `dir/bin/` and checks its SHA-256,
/// then runs the install step in `dir`. Another platform's binary is never
/// fetched. The step runs in Fiber's process group, so its terminal prompts
/// still work, and is stopped at [`INSTALL_STEP_DEADLINE`] on `clock`.
pub(crate) fn prepare(
    dir: &Path,
    manifest: &Manifest,
    clock: &dyn Clock,
    observe: &Observe<'_>,
) -> Result<(), Error> {
    prepare_binary(dir, manifest, observe)?;
    install_step(dir, manifest, clock)
}

/// Runs the manifest's install step in `dir`, stopped at
/// [`INSTALL_STEP_DEADLINE`] on `clock`. The step runs in Fiber's process
/// group, so its terminal prompts still work. [`prepare`] runs this after
/// the binary download; the update commit runs it without one, so every
/// member-binary download finishes before the first rename.
pub(crate) fn install_step(
    dir: &Path,
    manifest: &Manifest,
    clock: &dyn Clock,
) -> Result<(), Error> {
    if let Some(step) = &manifest.install
        && let Some((program, args)) = step.split_first()
    {
        run_step(dir, &manifest.name, step, program, args, clock)?;
    }
    Ok(())
}

/// Runs the manifest's install `step` in `dir`, stopped at
/// [`INSTALL_STEP_DEADLINE`] on `clock`.
fn run_step(
    dir: &Path,
    name: &str,
    step: &[String],
    program: &str,
    args: &[String],
    clock: &dyn Clock,
) -> Result<(), Error> {
    let failed = |why: String| Error::InstallStep {
        name: name.into(),
        why,
    };
    let req = exec::ExecRequest {
        program: program.into(),
        args: args.to_vec(),
        cwd: dir.to_path_buf(),
        // The step's output is not capped today.
        env: BTreeMap::new(),
        input: None,
        cap: usize::MAX,
        // In Fiber's process group, so terminal prompts still work and
        // Ctrl-C at the terminal still reaches the step as it does today.
        own_group: false,
        #[cfg(test)]
        test_events: None,
    };
    let deadline = clock
        .now()
        .checked_add(INSTALL_STEP_DEADLINE)
        .unwrap_or(clock.now());
    // Never cancelled except by the step's own end: the sender drops when
    // this returns.
    let (_cancel, cancel) = mpsc::channel::<()>();
    match exec::run(&req, clock, Some(deadline), cancel) {
        Err(failed_run) => match failed_run.source {
            Some(source) => Err(failed(format!("`{program}`: {source}"))),
            // After the spawn: a reader thread that could not start.
            None => Err(failed(failed_run.message)),
        },
        Ok(ran) if ran.timed_out => Err(Error::InstallExited {
            name: name.into(),
            why: format!(
                "`{}` did not finish within {} s, so it was stopped",
                step.join(" "),
                INSTALL_STEP_DEADLINE.as_secs()
            ),
        }),
        Ok(ran) if ran.exit_code != Some(0) => Err(Error::InstallExited {
            name: name.into(),
            why: format!(
                "`{}` failed: {}",
                step.join(" "),
                String::from_utf8_lossy(&ran.stderr).trim()
            ),
        }),
        Ok(_) => Ok(()),
    }
}

/// The file name the manifest `name` installs its binary under: the segment
/// after the last `/` (`docs/extensions.md`, "What a package holds").
fn binary_name(name: &str) -> Option<&str> {
    let segment = name.rsplit('/').next()?;
    (!segment.is_empty() && segment != "." && segment != "..").then_some(segment)
}

/// Downloads `url` with the production limits and the proxy the environment
/// names, as ureq's default config does. Nothing cancels a download, so its
/// `Keep` holds no socket and is never stopped. The download writes one
/// `extension_release` line on `observe`: `status` only when a response
/// arrived, the consumed body byte count however the read ends, and no
/// line when the send never left.
pub(crate) fn download_observed(
    name: &str,
    url: &str,
    observe: &Observe<'_>,
) -> Result<Vec<u8>, Error> {
    download_with(name, url, ureq::Proxy::try_from_env(), net::LIMITS, observe)
}

/// Downloads `url` over the shared connector, so a download that connects
/// to no address within the connect bound, or waits out the idle bound for
/// a byte, fails instead of hanging the install. Redirects are followed and
/// a non-2xx status is an error, as ureq's defaults have it; any failure is
/// the install's own `Download` error, not a connection error.
fn download_with(
    name: &str,
    url: &str,
    proxy: Option<ureq::Proxy>,
    limits: net::Limits,
    observe: &Observe<'_>,
) -> Result<Vec<u8>, Error> {
    let fail = |why: String| Error::Download {
        name: name.into(),
        why: format!("{url}: {why}"),
    };
    let agent = net::agent(
        net::config().proxy(proxy).build(),
        Arc::new(()),
        DefaultResolver::default(),
        limits,
    );
    // The send stamp: `None` when silent, so no clock is read at info.
    let start = observe.stamp();
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(error) => {
            // Post-send with a status keeps it; post-send without a
            // response carries no status or headers and zero bytes. A
            // send refusal never left, so it writes nothing.
            if let ureq::Error::StatusCode(status) = &error {
                observe.line(url, start, observe.stamp(), Some(*status), 0);
            } else {
                observe.line(url, start, None, None, 0);
            }
            return Err(fail(error.to_string()));
        }
    };
    let status = response.status().as_u16();
    let headers = observe.stamp();
    let mut reader = response.body_mut().with_config().limit(MAX_BINARY).reader();
    // Counted through `io::copy`: a read that fails keeps the bytes
    // consumed before it, and the line keeps them with the status.
    let mut bytes = Vec::new();
    match std::io::copy(&mut reader, &mut bytes) {
        Ok(consumed) => {
            observe.line(url, start, headers, Some(status), consumed);
            Ok(bytes)
        }
        Err(error) => {
            observe.line(url, start, headers, Some(status), bytes.len() as u64);
            Err(fail(error.to_string()))
        }
    }
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
