//! Controlling-terminal I/O (feature `tty`, Unix only).
//!
//! The only platform code in the crate. It uses `rustix`'s safe wrappers, so
//! it needs no `unsafe` either. Termios state changed by [`Tty::probe`] is
//! restored by a drop guard, which covers early returns, errors and panics.
//! It does *not* cover the process being killed by a signal during the probe;
//! a binary that cares installs its own handlers (the CLI does).

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags, open};
use rustix::io::Errno;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};

use crate::detect::{Detection, Detector, query_with_da1};

/// The controlling terminal, opened through `/dev/tty`. Writing here instead
/// of stdout means reports still reach the terminal when stdout is piped or
/// redirected.
#[derive(Debug)]
pub struct Tty {
    fd: OwnedFd,
}

impl AsFd for Tty {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Tty {
    /// Opens `/dev/tty` read-write. Returns `Ok(None)` when the process has no
    /// controlling terminal (`ENXIO`, `ENODEV` or `ENOENT`).
    pub fn open() -> io::Result<Option<Tty>> {
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        match open("/dev/tty", flags, Mode::empty()) {
            Ok(fd) => Ok(Some(Tty { fd })),
            Err(Errno::NXIO | Errno::NODEV | Errno::NOENT) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Wraps an already-open terminal file descriptor, such as a PTY.
    pub fn from_fd(fd: OwnedFd) -> Tty {
        Tty { fd }
    }

    /// Writes one complete sequence with a single `write(2)` call, so it
    /// can't interleave with other writers mid-sequence. The call is retried
    /// only on `EINTR`, when nothing was written. A short write is reported as
    /// an error rather than finished with a second write.
    pub fn write_sequence(&self, bytes: &[u8]) -> io::Result<()> {
        loop {
            match rustix::io::write(&self.fd, bytes) {
                Ok(n) if n == bytes.len() => return Ok(()),
                Ok(n) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("short write to tty: {n} of {} bytes", bytes.len()),
                    ));
                }
                Err(Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Runs feature detection (spec: *Feature detection*). It switches the tty
    /// to raw mode, sends the query and DA1 in one write, and reads replies
    /// until DA1 answers or `timeout` passes. Termios is restored on every
    /// return path.
    ///
    /// Reading continues after a positive answer until the DA1 reply arrives,
    /// so no reply bytes are left for the shell to read. Never returns
    /// [`Detection::Pending`]: no answer in time means unsupported.
    pub fn probe(&self, timeout: Duration) -> io::Result<Detection> {
        let original = tcgetattr(&self.fd)?;
        let mut raw = original.clone();
        raw.make_raw();
        tcsetattr(&self.fd, OptionalActions::Now, &raw)?;
        let _restore = Restore {
            fd: self.fd.as_fd(),
            termios: original,
        };

        self.write_sequence(&query_with_da1())?;

        let deadline = Instant::now() + timeout;
        let mut detector = Detector::new();
        let mut buf = [0u8; 512];
        while !detector.da1_seen() {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let ts = Timespec {
                tv_sec: left.as_secs() as _,
                tv_nsec: left.subsec_nanos() as _,
            };
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => break,
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
            match rustix::io::read(&self.fd, &mut buf[..]) {
                Ok(0) => break,
                Ok(n) => {
                    detector.feed(&buf[..n]);
                }
                Err(Errno::INTR | Errno::AGAIN) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(match detector.result() {
            Detection::Pending => Detection::Unsupported,
            other => other.clone(),
        })
    }
}

/// Restores saved termios on drop.
struct Restore<'a> {
    fd: BorrowedFd<'a>,
    termios: Termios,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        // Nothing useful can be done about a failure here.
        let _ = tcsetattr(self.fd, OptionalActions::Now, &self.termios);
    }
}
