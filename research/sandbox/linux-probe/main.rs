// Probe: live Landlock (Linux LSM) behaviour on GitHub-hosted runners.
// Ticket: Fiber #30.
//
// Usage: landlock-probe <allowed_dir> <shell_command>
//
// Restricts this process to:
//   - read access everywhere under "/"
//   - write access only under <allowed_dir> and /tmp
//   - (best-effort) TCP connect denied everywhere (no NetPort allow-rules)
// then execs `/bin/bash -c '<shell_command>'` so the restrictions apply to
// the exec'd command too. Prints the RestrictionStatus to stderr first so
// the caller can see what was actually enforced on this kernel.

use landlock::{
    Access, AccessFs, AccessNet, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
    ABI,
};
use std::env;
use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let program = args.next().unwrap_or_default();
    let allowed_dir = args
        .next()
        .ok_or_else(|| format!("usage: {program} <allowed_dir> <shell_command>"))?;
    let shell_command = args
        .next()
        .ok_or_else(|| format!("usage: {program} <allowed_dir> <shell_command>"))?;

    // The crate's own ABI cap. `handle_access` is best-effort by default
    // (Ruleset::default()'s CompatLevel is BestEffort), so requesting
    // rights the running kernel doesn't know about is silently downgraded
    // rather than an error.
    let abi = ABI::V9;

    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))?
        .handle_access(AccessNet::ConnectTcp)? // best-effort: no-op pre-ABI4
        .create()?
        .add_rule(PathBeneath::new(PathFd::new("/")?, AccessFs::from_read(abi)))?
        .add_rule(PathBeneath::new(
            PathFd::new(&allowed_dir)?,
            AccessFs::from_all(abi),
        ))?
        .add_rule(PathBeneath::new(
            PathFd::new("/tmp")?,
            AccessFs::from_all(abi),
        ))?
        // No NetPort::new(_, AccessNet::ConnectTcp) allow-rule is added, so
        // once ConnectTcp is handled, every TCP connect is denied.
        .restrict_self()?;

    eprintln!("landlock-probe: restriction status = {status:?}");

    // Replaces this process; the exec'd command inherits the ruleset.
    let err = Command::new("/bin/bash")
        .arg("-c")
        .arg(&shell_command)
        .exec();
    // exec() only returns on error.
    Err(Box::new(err))
}
