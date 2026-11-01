//! Asking the scheduler for quick turns on a busy machine.

use std::os::unix::process::CommandExt;
use std::process::Command;

/// How long a turn on the CPU Caret's threads ask for, a tenth of the usual. They run for
/// far less than this at a time, so they give up nothing for it. With 200 busy processes
/// on 12 CPUs, a key comes back on screen in 4 ms rather than 17, and 42 ms rather than 223
/// at worst.
const QUICK_TURN_NANOSECONDS: u64 = 300_000;

/// Leaves the thread's scheduling policy as it is.
const KEEP_POLICY: u64 = 0x08;

#[repr(C)]
#[derive(Default)]
struct SchedAttr {
    size: u32,
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
    utilization_min: u32,
    utilization_max: u32,
}

/// Has the calling thread, and the threads it starts from now on, scheduled soon after they
/// wake while every CPU is taken. What a program writes passes through several threads on
/// its way to the screen, each waiting for a CPU in turn, so under load its output would
/// crawl while the window's own bars stay quick.
///
/// A shorter turn needs no privilege, as a higher priority would, and gives no more than a
/// fair share of the CPU. On a kernel without it (before 6.12) this changes nothing.
pub fn ask_for_quick_turns() {
    set_turn(QUICK_TURN_NANOSECONDS);
}

/// Gives the calling thread the usual turns back, as a program started from Caret has to
/// have: it would keep the short ones, and a build in a terminal would take the advantage
/// back from what shows its output. Only makes system calls, so it can run between fork
/// and exec.
pub fn ask_for_usual_turns() {
    set_turn(0);
}

/// Has `command` start with the usual turns.
#[expect(unsafe_code, reason = "pre_exec runs code between fork and exec")]
pub fn start_with_usual_turns(command: &mut Command) -> &mut Command {
    // SAFETY: what runs between fork and exec is getpriority and sched_setattr, which are
    // async-signal-safe, and it allocates nothing.
    unsafe {
        command.pre_exec(|| {
            ask_for_usual_turns();
            Ok(())
        })
    }
}

#[expect(unsafe_code, reason = "sched_setattr is only reached through libc")]
fn set_turn(runtime: u64) {
    // SAFETY: getpriority takes no pointers; on Linux, 0 is the calling thread.
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    let attr = SchedAttr {
        size: size_of::<SchedAttr>() as u32,
        flags: KEEP_POLICY,
        nice,
        runtime,
        ..SchedAttr::default()
    };
    // SAFETY: the pointer is to a value of the size it gives, alive through the call; pid 0
    // is the calling thread. A failure leaves the thread as it was, which is all this can do.
    let _ = unsafe { libc::syscall(libc::SYS_sched_setattr, 0, &raw const attr, 0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length of the calling process's turn, on a kernel that tells.
    fn own_turn() -> Option<String> {
        turn_of(&std::fs::read_to_string("/proc/thread-self/sched").ok()?)
    }

    fn turn_of(sched: &str) -> Option<String> {
        let line = sched.lines().find(|line| line.starts_with("se.slice"))?;
        line.split_whitespace().last().map(str::to_owned)
    }

    #[test]
    fn a_program_started_with_the_usual_turns_does_not_keep_quick_ones() {
        std::thread::spawn(|| {
            let before = own_turn();
            ask_for_quick_turns();
            let quick = own_turn();
            if before.is_none() || quick == before {
                // This kernel does not have them, or does not say.
                return;
            }
            let mut program = Command::new("cat");
            program.arg("/proc/self/sched");
            let output = start_with_usual_turns(&mut program)
                .output()
                .expect("cat starts");
            let told = String::from_utf8_lossy(&output.stdout);
            assert_ne!(turn_of(&told), quick);
            let unchanged = Command::new("cat").arg("/proc/self/sched").output();
            let told = String::from_utf8_lossy(&unchanged.expect("cat starts").stdout).into_owned();
            assert_eq!(turn_of(&told), quick, "one started plainly inherits them");
        })
        .join()
        .expect("the thread's checks hold");
    }
}
