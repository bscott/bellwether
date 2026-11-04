//! Signal handling for `run` and `probe`.
//!
//! No signal handlers are installed. Instead, the signals bellwether cares
//! about are *blocked* in every thread, and one thread takes them with
//! `sigwait(2)`. No code ever runs in signal context.
//!
//! A blocked mask is inherited across `fork` and `exec`, and
//! `std::process::Command` does *not* reset it (only SIGPIPE's disposition).
//! [`Hold::unblock_in_child`] restores the original mask in the child just
//! before `exec`, so the child still gets Ctrl-C and friends normally.
//!
//! There are two `unsafe` calls, in [`Hold::unblock_in_child`] and
//! [`reset_default`]. Each has a SAFETY comment.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use nix::sys::signal::{SigHandler, SigSet, SigmaskHow, Signal, kill, pthread_sigmask, raise};
use nix::unistd::Pid;

/// The signals `run` takes over while the child runs, and `probe` holds back
/// while the terminal is in raw mode.
pub fn handled() -> SigSet {
    let mut set = SigSet::empty();
    for s in [
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTERM,
        Signal::SIGHUP,
    ] {
        set.add(s);
    }
    set
}

/// Blocks [`handled`] signals in the calling thread until dropped. Signals
/// that arrive meanwhile stay pending and are delivered when the old mask is
/// restored. Must be created before any other thread is spawned, so that
/// threads inherit the mask.
pub struct Hold {
    old: SigSet,
}

impl Hold {
    pub fn new() -> Hold {
        let mut old = SigSet::empty();
        // pthread_sigmask can only fail on an invalid `how`.
        let _ = pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&handled()), Some(&mut old));
        Hold { old }
    }

    /// Makes `cmd`'s child start with the signal mask that was in effect before
    /// this `Hold`, instead of inheriting the blocked one.
    #[allow(unsafe_code)]
    pub fn unblock_in_child(&self, cmd: &mut Command) {
        let old = self.old;
        // SAFETY: the closure runs in the forked child before `exec`, where only
        // async-signal-safe operations are allowed. It makes one
        // `pthread_sigmask` call (async-signal-safe) on a `SigSet` copied in
        // beforehand. No allocation, no locks, and nothing shared with the parent.
        unsafe {
            cmd.pre_exec(move || {
                pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&old), None)
                    .map_err(|e| io::Error::from_raw_os_error(e as i32))
            });
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.old), None);
    }
}

#[derive(Default)]
struct Shared {
    /// Set once the child has been reaped. Forwarding stops then.
    reaped: bool,
}

/// Owns the `sigwait` thread for `run`. SIGINT and SIGQUIT are swallowed: the
/// child is in the terminal's foreground process group and already got them
/// from the tty driver. SIGTERM and SIGHUP are forwarded to the child, because
/// they were sent to bellwether's pid alone.
pub struct Forwarder {
    shared: Arc<Mutex<Shared>>,
}

impl Forwarder {
    /// Starts the thread. The calling thread must already hold a [`Hold`].
    pub fn start(child: u32) -> Forwarder {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let theirs = Arc::clone(&shared);
        let pid = Pid::from_raw(child as i32);
        thread::spawn(move || {
            let set = handled();
            loop {
                let Ok(sig) = set.wait() else { continue };
                if matches!(sig, Signal::SIGTERM | Signal::SIGHUP) {
                    let s = theirs.lock().unwrap_or_else(|e| e.into_inner());
                    if !s.reaped {
                        let _ = kill(pid, sig);
                    }
                }
                // SIGINT / SIGQUIT: ignored on purpose.
            }
        });
        Forwarder { shared }
    }

    /// Call right after the child is reaped. A SIGTERM taken by the forwarder
    /// after this point isn't forwarded. There remains a microsecond window
    /// between `waitpid` returning and this call in which the pid could in
    /// theory be reused. Closing it needs `waitid(WNOWAIT)`, which macOS lacks.
    pub fn child_reaped(&self) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.reaped = true;
    }
}

/// Re-raises `sig` with its default disposition so the caller (a shell,
/// `make`) sees a real signal death, then falls back to exit status 128+N if
/// we are somehow still alive (for example, a signal whose default action
/// isn't to terminate).
pub fn die_by_signal(sig: Signal) -> ! {
    reset_default(sig);
    let _ = raise(sig);
    let mut one = SigSet::empty();
    one.add(sig);
    // If `sig` is one of the blocked ones, it is now pending on this thread.
    // Unblocking delivers it.
    let _ = pthread_sigmask(SigmaskHow::SIG_UNBLOCK, Some(&one), None);
    std::process::exit(128 + sig as i32)
}

/// Sets `sig` to `SIG_DFL`. bellwether never installs handlers, but the Rust
/// runtime sets SIGPIPE to `SIG_IGN` at startup, so re-raising SIGPIPE would
/// otherwise do nothing.
#[allow(unsafe_code)]
fn reset_default(sig: Signal) {
    // SAFETY: `signal` is unsafe because an installed handler runs in signal
    // context and must be async-signal-safe. `SigDfl` installs no handler
    // (the kernel takes the default action), so that requirement doesn't
    // apply, and no other thread relies on the previous disposition.
    let _ = unsafe { nix::sys::signal::signal(sig, SigHandler::SigDfl) };
}
