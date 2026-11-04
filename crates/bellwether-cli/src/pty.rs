//! `run --pty`: run the command in a pseudoterminal and watch its output.
//!
//! bellwether sits between the terminal and the command, like `script(1)`:
//!
//! * Keystrokes are copied to the PTY unchanged, with our terminal in raw mode,
//!   so Ctrl-C, Ctrl-Z and line editing are handled by the PTY's line
//!   discipline for the command.
//! * Output is copied back unchanged. On the way it goes through the
//!   [`Scanner`] (to notice the command's own OSC 7501 reports) and the
//!   [`Watcher`] (to find progress and prompts with the presets).
//! * SIGWINCH copies our terminal's size to the PTY. SIGINT and SIGQUIT sent
//!   to bellwether go to the PTY's foreground process group, as if typed.
//!   SIGTERM and SIGHUP go to the command.
//!
//! Everything runs on one thread around `poll(2)`, except the `sigwait`
//! forwarder and a thread that waits for the command to exit.

use std::io::{self, Read as _, Write as _};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use bellwether::{Event, Id, Kind, Message, Scanner, State};
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags, fcntl_getfl, fcntl_setfl, open};
use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{
    LocalModes, OptionalActions, Termios, Winsize, isatty, tcgetattr, tcgetpgrp, tcgetwinsize,
    tcsetattr, tcsetwinsize,
};

use crate::run::{Failure, Reporter};
use crate::sys::{self, Forwarder};
use crate::watch::{Rules, Status, Watcher};

/// Minimum gap between reports that only change `progress`.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How long a report may wait for the output to reach a clean boundary (see
/// [`Watcher::at_boundary`]) before it is written anyway.
const BOUNDARY_WAIT: Duration = Duration::from_millis(250);
/// After the command exits, keep copying output until it has been quiet this
/// long, or the PTY reports end of file...
const DRAIN_IDLE: Duration = Duration::from_millis(100);
/// ...but no longer than this, in case a background process keeps the PTY open.
const DRAIN_MAX: Duration = Duration::from_secs(1);

/// Runs `cmd` in a new PTY until it exits. Reports `working` / `blocked` /
/// progress through `reporter` as the output changes; the caller reports the
/// final state. `shown` is what the caller already reported. `fallback` is the
/// record to move to if the command reports on ours.
pub fn run(
    mut cmd: Command,
    reporter: &mut Reporter,
    rules: Rules,
    shown: Shown,
    fallback: Option<Id>,
) -> Result<ExitStatus, Failure> {
    let stdin = rustix::stdio::stdin();
    let stdout = rustix::stdio::stdout();
    let stdin_tty = isatty(stdin);

    // The terminal whose modes and size the PTY starts with.
    let term = [stdin, stdout]
        .into_iter()
        .find(|fd| isatty(fd))
        .and_then(|fd| fd.try_clone_to_owned().ok());
    let modes = term.as_ref().and_then(|t| tcgetattr(t).ok());
    let size = term
        .as_ref()
        .and_then(|t| tcgetwinsize(t).ok())
        .filter(|w| w.ws_row > 0 && w.ws_col > 0)
        .unwrap_or(Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        });

    // Piped input isn't typed by a person: don't echo it into the output.
    let (master, secondary) = open_pty(modes.as_ref(), size, stdin_tty).map_err(Failure::Setup)?;
    let stdio = || {
        secondary
            .try_clone()
            .map(Stdio::from)
            .map_err(Failure::Setup)
    };
    cmd.stdin(stdio()?).stdout(stdio()?).stderr(stdio()?);
    sys::session_leader(&mut cmd);
    let mut child = cmd.spawn().map_err(Failure::Spawn)?;
    // Only the command may hold the secondary side, so its exit ends the output.
    drop(cmd);
    drop(secondary);
    let pid = Pid::from_raw(child.id() as i32);

    let flags = fcntl_getfl(&master).map_err(io_err)?;
    fcntl_setfl(&master, flags | OFlags::NONBLOCK).map_err(io_err)?;
    let master = Arc::new(master);

    let _raw = if stdin_tty {
        RawMode::enter().ok()
    } else {
        None
    };

    sys::make_waitable(Signal::SIGWINCH);
    let forwarder = Arc::new(Forwarder::start(sys::with(&[Signal::SIGWINCH]), {
        let master = Arc::clone(&master);
        move |sig| match sig {
            Signal::SIGWINCH => {
                if let Some(ws) = term.as_ref().and_then(|t| tcgetwinsize(t).ok()) {
                    // The kernel sends SIGWINCH to the command's foreground group.
                    let _ = tcsetwinsize(&*master, ws);
                }
            }
            Signal::SIGINT | Signal::SIGQUIT => {
                let group = tcgetpgrp(&*master)
                    .map(|p| Pid::from_raw(p.as_raw_nonzero().get()))
                    .unwrap_or(pid);
                let _ = killpg(group, sig);
            }
            _ => {
                let _ = kill(pid, sig);
            }
        }
    }));

    let (wake_rx, mut wake_tx) = UnixStream::pair().map_err(Failure::Setup)?;
    wake_rx.set_nonblocking(true).map_err(Failure::Setup)?;
    let (tx, rx) = mpsc::channel();
    {
        let forwarder = Arc::clone(&forwarder);
        thread::spawn(move || {
            let status = loop {
                match child.wait() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    other => break other,
                }
            };
            forwarder.child_reaped();
            let _ = tx.send(status);
            let _ = wake_tx.write_all(&[1]);
        });
    }

    let mut session = Session {
        master,
        reporter,
        fallback,
        moved: false,
        silent: false,
        watcher: Watcher::new(rules),
        scanner: Scanner::new(),
        working_msg: shown.msg.clone(),
        shown: Some(shown),
        last_sent: None,
        waiting_since: None,
        output_ok: true,
        pid,
    };
    session.pump(&wake_rx, &rx, stdin_tty)
}

/// What a status report says, to tell whether the next one differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub msg: String,
}

struct Session<'a> {
    master: Arc<OwnedFd>,
    reporter: &'a mut Reporter,
    fallback: Option<Id>,
    /// Moved off the original record already.
    moved: bool,
    /// The command reported on our record and there was nowhere to move.
    silent: bool,
    watcher: Watcher,
    scanner: Scanner,
    working_msg: String,
    shown: Option<Shown>,
    last_sent: Option<Instant>,
    /// A report has been waiting for an output boundary since then.
    waiting_since: Option<Instant>,
    /// Stdout still accepts writes.
    output_ok: bool,
    pid: Pid,
}

impl Session<'_> {
    fn pump(
        &mut self,
        wake: &UnixStream,
        exits: &mpsc::Receiver<io::Result<ExitStatus>>,
        stdin_tty: bool,
    ) -> Result<ExitStatus, Failure> {
        let stdin = rustix::stdio::stdin();
        let mut stdin_open = true;
        let mut master_open = true;
        let mut to_child: Vec<u8> = Vec::new();
        let mut exited: Option<io::Result<ExitStatus>> = None;
        let mut drain_idle: Option<Instant> = None;
        let mut drain_max: Option<Instant> = None;
        let mut retry: Option<Instant> = None;
        let mut buf = vec![0u8; 16 * 1024];
        let ready = PollFlags::IN | PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL;

        loop {
            let now = Instant::now();
            if exited.is_some()
                && (!master_open
                    || drain_idle.is_some_and(|d| now >= d)
                    || drain_max.is_some_and(|d| now >= d))
            {
                break;
            }
            let running = exited.is_none();
            let deadline = [self.watcher.deadline(), retry, drain_idle, drain_max]
                .into_iter()
                .flatten()
                .min();

            let (wake_ev, master_ev, stdin_ev) = {
                let mut fds = vec![PollFd::new(wake, PollFlags::IN)];
                let mut master_at = None;
                if master_open {
                    let mut f = PollFlags::IN;
                    if running && !to_child.is_empty() {
                        f |= PollFlags::OUT;
                    }
                    fds.push(PollFd::new(&*self.master, f));
                    master_at = Some(fds.len() - 1);
                }
                // Once the command has exited, stop reading the keyboard: keys
                // typed now belong to the shell.
                let mut stdin_at = None;
                if stdin_open && running && to_child.len() < 64 * 1024 {
                    fds.push(PollFd::new(&stdin, PollFlags::IN));
                    stdin_at = Some(fds.len() - 1);
                }
                let timeout = deadline.map(|d| timespec(d.saturating_duration_since(now)));
                match poll(&mut fds, timeout.as_ref()) {
                    Ok(_) => {}
                    Err(Errno::INTR) => continue,
                    Err(e) => return Err(Failure::Wait(e.into())),
                }
                let ev = |at: Option<usize>| at.map_or(PollFlags::empty(), |i| fds[i].revents());
                (fds[0].revents(), ev(master_at), ev(stdin_at))
            };
            let now = Instant::now();

            if wake_ev.intersects(ready) {
                let mut sink = [0u8; 16];
                while matches!((&*wake).read(&mut sink), Ok(n) if n > 0) {}
                if let Ok(status) = exits.try_recv() {
                    exited = Some(status);
                    drain_idle = Some(now + DRAIN_IDLE);
                    drain_max = Some(now + DRAIN_MAX);
                }
            }

            if master_ev.intersects(ready) {
                match rustix::io::read(&*self.master, &mut buf) {
                    // EOF on macOS, EIO on Linux: nothing holds the secondary side.
                    Ok(0) | Err(Errno::IO) => master_open = false,
                    Ok(n) => {
                        self.output(&buf[..n], now);
                        if exited.is_some() {
                            drain_idle = Some(now + DRAIN_IDLE);
                        }
                    }
                    Err(Errno::AGAIN | Errno::INTR) => {}
                    Err(_) => master_open = false,
                }
            }

            if stdin_ev.intersects(ready) {
                match rustix::io::read(stdin, &mut buf) {
                    Ok(0) => {
                        stdin_open = false;
                        // Piped input ran out: pass the end on, as typing Ctrl-D
                        // would. A terminal never reads 0 bytes in raw mode.
                        if !stdin_tty {
                            to_child.push(0x04);
                        }
                    }
                    Ok(n) => {
                        self.watcher.input(&buf[..n]);
                        to_child.extend_from_slice(&buf[..n]);
                    }
                    Err(Errno::AGAIN | Errno::INTR) => {}
                    Err(_) => stdin_open = false,
                }
            }

            if master_open && running && !to_child.is_empty() {
                match rustix::io::write(&*self.master, &to_child) {
                    Ok(n) => {
                        to_child.drain(..n);
                    }
                    Err(Errno::AGAIN | Errno::INTR) => {}
                    Err(_) => to_child.clear(),
                }
            }

            self.watcher.tick(now);
            retry = self.sync(now);
        }

        let mut rest = Vec::new();
        self.scanner.flush(|ev| {
            if let Event::Bytes(b) = ev {
                rest.extend_from_slice(b);
            }
        });
        self.write_out(&rest);
        exited.expect("loop ends after exit").map_err(Failure::Wait)
    }

    /// Copies one chunk of command output to stdout, scanning it on the way.
    fn output(&mut self, chunk: &[u8], now: Instant) {
        let mut collided = false;
        let mut out: Vec<u8> = Vec::with_capacity(chunk.len());
        let ours = self.reporter.id.clone();
        let watcher = &mut self.watcher;
        self.scanner.feed(chunk, |ev| match ev {
            Event::Bytes(b) => {
                watcher.feed(b);
                out.extend_from_slice(b);
            }
            Event::Sequence { raw, message, .. } => {
                // The command's own report goes to the terminal untouched. If it
                // is on the record we are using, it just replaced ours.
                if let Message::Report(Ok(r)) = &message
                    && r.id() == ours.as_ref()
                {
                    collided = true;
                }
                out.extend_from_slice(raw);
            }
            Event::Oversized => {}
        });
        self.watcher.pause(now);
        self.write_out(&out);
        if collided && !self.moved {
            self.moved = true;
            match self.fallback.take() {
                Some(id) => {
                    self.reporter.id = Some(id);
                    self.shown = None;
                }
                None => self.silent = true,
            }
        }
    }

    fn write_out(&mut self, bytes: &[u8]) {
        if self.output_ok && !bytes.is_empty() && write_all(rustix::stdio::stdout(), bytes).is_err()
        {
            // Whatever read our output is gone (`| head`). Treat it like a
            // terminal hangup: the command gets SIGHUP.
            self.output_ok = false;
            let _ = killpg(self.pid, Signal::SIGHUP);
        }
    }

    fn wanted(&self) -> Shown {
        let progress = self.watcher.progress();
        match self.watcher.status() {
            Status::Working => Shown {
                state: State::Working,
                kind: None,
                progress,
                msg: self.working_msg.clone(),
            },
            Status::Blocked { kind, msg } => Shown {
                state: State::Blocked,
                kind: Some(kind),
                progress,
                msg,
            },
        }
    }

    /// Reports the current status if it changed. Returns when to call again if
    /// a report is being held back.
    fn sync(&mut self, now: Instant) -> Option<Instant> {
        if self.silent {
            return None;
        }
        let want = self.wanted();
        if self.shown.as_ref() == Some(&want) {
            self.waiting_since = None;
            return None;
        }
        // Never write into the middle of the command's escape sequence.
        if !self.watcher.at_boundary() {
            let since = *self.waiting_since.get_or_insert(now);
            if now < since + BOUNDARY_WAIT {
                return Some(since + BOUNDARY_WAIT);
            }
        }
        let progress_only = self
            .shown
            .as_ref()
            .is_some_and(|s| s.state == want.state && s.kind == want.kind && s.msg == want.msg);
        if progress_only
            && let Some(last) = self.last_sent
            && now < last + PROGRESS_INTERVAL
        {
            return Some(last + PROGRESS_INTERVAL);
        }
        self.reporter
            .send(want.state, want.kind, want.progress, &want.msg);
        self.shown = Some(want);
        self.last_sent = Some(now);
        self.waiting_since = None;
        None
    }
}

/// Opens a PTY pair with the given modes and size, and echo on or off. Both
/// ends are close-on-exec; the command gets the secondary side through its
/// stdio.
fn open_pty(modes: Option<&Termios>, size: Winsize, echo: bool) -> io::Result<(OwnedFd, OwnedFd)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let secondary = open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut m = match modes {
        Some(m) => m.clone(),
        None => tcgetattr(&secondary)?,
    };
    if !echo {
        m.local_modes.remove(LocalModes::ECHO);
    }
    tcsetattr(&secondary, OptionalActions::Now, &m)?;
    tcsetwinsize(&secondary, size)?;
    Ok((master, secondary))
}

/// Our terminal (stdin) in raw mode until dropped.
struct RawMode {
    saved: Termios,
}

impl RawMode {
    fn enter() -> io::Result<RawMode> {
        let fd = rustix::stdio::stdin();
        let saved = tcgetattr(fd)?;
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(fd, OptionalActions::Now, &raw)?;
        Ok(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(rustix::stdio::stdin(), OptionalActions::Now, &self.saved);
    }
}

/// Writes all of `bytes`, waiting if `fd` is non-blocking and full.
fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => {
                let mut fds = [PollFd::new(&fd, PollFlags::OUT)];
                let _ = poll(&mut fds, None);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: d.as_secs() as _,
        tv_nsec: d.subsec_nanos() as _,
    }
}

fn io_err(e: Errno) -> Failure {
    Failure::Setup(e.into())
}
