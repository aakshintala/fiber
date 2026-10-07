//! macOS only: a re-run child gets no crash exception port, so the kernel
//! never hands its deliberate `abort()` to ReportCrash, which would write a
//! `.ips` report into `~/Library/Logs/DiagnosticReports` on every run. Task
//! exception ports survive exec, so the binary under test is unchanged.

use std::os::unix::process::CommandExt as _;
use std::process::Command;

/// `EXC_MASK_CRASH` (1 << 10, the fatal-signal exception that `abort()`
/// raises) and `EXC_MASK_CORPSE_NOTIFY` (1 << 13), the two exception types
/// that reach ReportCrash.
const CRASH_MASK: u32 = 0x2400;

/// `EXCEPTION_DEFAULT`.
const BEHAVIOR: i32 = 1;

/// `THREAD_STATE_NONE`, which differs by CPU.
#[cfg(target_arch = "aarch64")]
const FLAVOR: i32 = 5;
#[cfg(not(target_arch = "aarch64"))]
const FLAVOR: i32 = 13;

#[allow(
    unsafe_code,
    reason = "declares the Mach calls that set and read the task's crash exception port"
)]
unsafe extern "C" {
    static mach_task_self_: u32;
    fn task_set_exception_ports(
        task: u32,
        exception_mask: u32,
        new_port: u32,
        behavior: i32,
        new_flavor: i32,
    ) -> i32;
}

/// Makes `command`'s child drop its crash exception port between fork and
/// exec. A failed call leaves the inherited port, so the child still aborts,
/// only with a report.
// Mutants run on Linux, where this macOS-only module is not compiled, so no
// test there can see a mutant of this function; `a_rerun_child_has_no_crash_
// exception_port` fails on macOS when it does nothing.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn silence(command: &mut Command) {
    #[allow(
        unsafe_code,
        reason = "pre_exec is the only way to run code in the child before exec"
    )]
    // SAFETY: between fork and exec the closure makes one Mach trap on the
    // child's own task; it allocates nothing and takes no lock.
    unsafe {
        command.pre_exec(|| {
            task_set_exception_ports(mach_task_self_, CRASH_MASK, 0, BEHAVIOR, FLAVOR);
            Ok(())
        });
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "test helpers around the Mach calls that set and read the crash exception port"
)]
pub(crate) mod probe {
    use super::*;

    unsafe extern "C" {
        fn mach_port_allocate(task: u32, right: u32, name: *mut u32) -> i32;
        fn mach_port_insert_right(task: u32, name: u32, poly: u32, poly_type: u32) -> i32;
        fn task_get_exception_ports(
            task: u32,
            exception_mask: u32,
            masks: *mut u32,
            count: *mut u32,
            handlers: *mut u32,
            behaviors: *mut i32,
            flavors: *mut i32,
        ) -> i32;
    }

    /// Gives `command`'s child a live crash exception port nobody reads, so a
    /// child that does not drop it is visible in [`handlers`]. Registered
    /// before [`silence`], it runs first.
    pub(crate) fn inherit_a_live_port(command: &mut Command) {
        // SAFETY: as in `silence`; the three Mach calls act on the child's
        // own task.
        unsafe {
            command.pre_exec(|| {
                let mut port = 0;
                // MACH_PORT_RIGHT_RECEIVE, then MACH_MSG_TYPE_MAKE_SEND.
                mach_port_allocate(mach_task_self_, 1, &mut port);
                mach_port_insert_right(mach_task_self_, port, port, 20);
                task_set_exception_ports(mach_task_self_, CRASH_MASK, port, BEHAVIOR, FLAVOR);
                Ok(())
            });
        }
    }

    /// How many crash exception ports this process has.
    pub(crate) fn handlers() -> usize {
        const SLOTS: u32 = 32;
        let mut masks = [0u32; SLOTS as usize];
        let mut ports = [0u32; SLOTS as usize];
        let mut behaviors = [0i32; SLOTS as usize];
        let mut flavors = [0i32; SLOTS as usize];
        let mut count = SLOTS;
        // SAFETY: every array holds `SLOTS` entries, the capacity `count`
        // announces.
        unsafe {
            task_get_exception_ports(
                mach_task_self_,
                CRASH_MASK,
                masks.as_mut_ptr(),
                &mut count,
                ports.as_mut_ptr(),
                behaviors.as_mut_ptr(),
                flavors.as_mut_ptr(),
            );
        }
        ports
            .iter()
            .take(count as usize)
            .filter(|p| **p != 0)
            .count()
    }
}
