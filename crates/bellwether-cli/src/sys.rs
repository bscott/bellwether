//! Signal and process-setup helpers for `run` and `probe`.
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
//! There are four `unsafe` calls, in [`Hold::unblock_in_child`],
//! [`session_leader`], [`make_waitable`] and [`reset_default`]. Each has a
//! SAFETY comment.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use nix::sys::signal::{
    SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, kill, pthread_sigmask, raise,
    sigaction,
};
use nix::unistd::Pid;

/// The signals `run` takes over while the child runs, and `probe` holds back
/// while the terminal is in raw mode.
pub fn handled() -> SigSet {
    with(&[])
}

/// [`handled`] plus `extra`.
pub fn with(extra: &[Signal]) -> SigSet {
    let mut set = SigSet::empty();
    for s in [
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTERM,
        Signal::SIGHUP,
    ]
    .iter()
    .chain(extra)
    {
        set.add(*s);
    }
    set
}

/// Blocks a set of signals in the calling thread until dropped. Signals
/// that arrive meanwhile stay pending and are delivered when the old mask is
/// restored. Must be created before any other thread is spawned, so that
/// threads inherit the mask.
pub struct Hold {
    old: SigSet,
}

impl Hold {
    pub fn new(set: SigSet) -> Hold {
        let mut old = SigSet::empty();
        // pthread_sigmask can only fail on an invalid `how`.
        let _ = pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), Some(&mut old));
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

/// Makes `cmd`'s child the leader of a new session whose controlling
/// terminal is its stdin. Used by `run --pty`, where stdin is the PTY's
/// secondary side: the child then gets job control, `/dev/tty` and
/// terminal-generated signals from the PTY instead of from our terminal.
#[allow(unsafe_code)]
pub fn session_leader(cmd: &mut Command) {
    // SAFETY: runs in the forked child before `exec`, after std has set up
    // stdio. `setsid` and `ioctl(TIOCSCTTY)` are plain syscalls
    // (async-signal-safe), there is no allocation or locking, and
    // `rustix::stdio::stdin()` only names fd 0.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
            Ok(())
        });
    }
}

/// Makes a signal whose default action is to be ignored (SIGWINCH) reach
/// `sigwait`. macOS discards such a signal when it arrives with the default
/// disposition, even while it is blocked and a thread waits for it; Linux
/// keeps it pending. Installing a handler changes the disposition. The handler
/// never runs: the signal stays blocked in every thread and `sigwait` takes it.
/// `exec` resets it to the default in the child.
#[allow(unsafe_code)]
pub fn make_waitable(sig: Signal) {
    extern "C" fn nothing(_: std::ffi::c_int) {}
    let action = SigAction::new(
        SigHandler::Handler(nothing),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: `sigaction` is unsafe because the handler runs in signal context
    // and must be async-signal-safe. `nothing` has an empty body, so it is.
    let _ = unsafe { sigaction(sig, &action) };
}

/// Owns the `sigwait` thread for `run`. Each signal in the set it was started
/// with is passed to a callback, unless the child has been reaped.
pub struct Forwarder {
    /// Set once the child has been reaped. Forwarding stops then.
    reaped: Arc<Mutex<bool>>,
}

impl Forwarder {
    /// Starts the thread. The calling thread must already hold a [`Hold`] for
    /// at least `set`, so no other thread takes these signals.
    pub fn start(set: SigSet, mut on: impl FnMut(Signal) + Send + 'static) -> Forwarder {
        let reaped = Arc::new(Mutex::new(false));
        let theirs = Arc::clone(&reaped);
        thread::spawn(move || {
            loop {
                let Ok(sig) = set.wait() else { continue };
                // The lock is held while `on` runs, so `child_reaped` can't
                // return while a `kill` is in flight.
                let reaped = theirs.lock().unwrap_or_else(|e| e.into_inner());
                if !*reaped {
                    on(sig);
                }
            }
        });
        Forwarder { reaped }
    }

    /// The forwarder for plain `run`. SIGINT and SIGQUIT are swallowed: the
    /// child is in the terminal's foreground process group and already got them
    /// from the tty driver. SIGTERM and SIGHUP are forwarded to the child,
    /// because they were sent to bellwether's pid alone.
    pub fn plain(child: u32) -> Forwarder {
        let pid = Pid::from_raw(child as i32);
        Forwarder::start(handled(), move |sig| {
            if matches!(sig, Signal::SIGTERM | Signal::SIGHUP) {
                let _ = kill(pid, sig);
            }
            // SIGINT / SIGQUIT: ignored on purpose.
        })
    }

    /// Call right after the child is reaped. A signal taken by the forwarder
    /// after this point isn't forwarded. There remains a microsecond window
    /// between `waitpid` returning and this call in which the pid could in
    /// theory be reused. Closing it needs `waitid(WNOWAIT)`, which macOS lacks.
    pub fn child_reaped(&self) {
        *self.reaped.lock().unwrap_or_else(|e| e.into_inner()) = true;
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
