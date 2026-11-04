//! `Tty::probe` against a fake terminal on a PTY master.
#![cfg(feature = "tty")]

use std::os::fd::OwnedFd;
use std::thread;
use std::time::{Duration, Instant};

use bellwether::detect::{Detection, QUERY, query_with_da1};
use bellwether::tty::Tty;
use rustix::fs::{Mode, OFlags, open};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::tcgetattr;

fn pty() -> (OwnedFd, OwnedFd) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY,
        Mode::empty(),
    )
    .unwrap();
    (master, slave)
}

/// Plays the terminal: waits for the query and DA1, then writes `reply`.
fn fake_terminal(master: OwnedFd, reply: &'static [u8]) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let want = query_with_da1();
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        while got.len() < want.len() {
            let n = rustix::io::read(&master, &mut buf[..]).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        if !reply.is_empty() {
            rustix::io::write(&master, reply).unwrap();
        }
        // Keep the master open until the probe has finished reading.
        thread::sleep(Duration::from_millis(300));
        got
    })
}

fn run(reply: &'static [u8], timeout: Duration) -> (Detection, Duration) {
    let (master, slave) = pty();
    let slave_dup = rustix::io::dup(&slave).unwrap();
    let before = tcgetattr(&slave).unwrap();
    let term = fake_terminal(master, reply);
    let tty = Tty::from_fd(slave_dup);
    let start = Instant::now();
    let result = tty.probe(timeout).unwrap();
    let elapsed = start.elapsed();
    let after = tcgetattr(&slave).unwrap();
    // PENDIN is kernel-managed state ("reprint pending input"), which macOS
    // sets when a tty returns to canonical mode. It isn't something we restore.
    let pendin = rustix::termios::LocalModes::PENDIN;
    assert_eq!(
        before.local_modes - pendin,
        after.local_modes - pendin,
        "termios restored"
    );
    assert_eq!(before.input_modes, after.input_modes, "termios restored");
    let sent = term.join().unwrap();
    assert!(sent.starts_with(QUERY), "query sent first: {sent:?}");
    (result, elapsed)
}

#[test]
fn supported_terminal() {
    let (r, elapsed) = run(
        b"\x1b]7501;?:kinds=auth\x1b\\\x1b[?62;22c",
        Duration::from_secs(2),
    );
    match r {
        Detection::Supported(s) => {
            assert_eq!(s.kinds, ["auth"]);
            assert_eq!(s.states.len(), 5);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(1),
        "returned on DA1, not timeout"
    );
}

#[test]
fn unsupported_terminal_answers_da1_only() {
    let (r, elapsed) = run(b"\x1b[?1;2c", Duration::from_secs(2));
    assert_eq!(r, Detection::Unsupported);
    assert!(elapsed < Duration::from_secs(1));
}

#[test]
fn silent_terminal_times_out() {
    let (r, elapsed) = run(b"", Duration::from_millis(100));
    assert_eq!(r, Detection::Unsupported);
    assert!(elapsed >= Duration::from_millis(100));
    assert!(elapsed < Duration::from_secs(1));
}
