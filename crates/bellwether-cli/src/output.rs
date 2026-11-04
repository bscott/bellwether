//! Where reports go: /dev/tty, stdout, or nowhere.

use std::io;

use bellwether::tty::{self, Tty};
use bellwether::{Report, Terminator};

/// The output target for reports.
pub enum Sink {
    /// The controlling terminal.
    Tty(Tty),
    /// Standard output (`--stdout`).
    Stdout,
    /// Output disabled (`BELLWETHER=0`), or no controlling terminal.
    Off,
}

impl Sink {
    /// Picks the target. `BELLWETHER=0` wins over everything, then `--stdout`,
    /// then `/dev/tty`. With no controlling terminal the sink is `Off`.
    pub fn open(stdout: bool) -> io::Result<Sink> {
        if std::env::var_os("BELLWETHER").is_some_and(|v| v == "0") {
            return Ok(Sink::Off);
        }
        if stdout {
            return Ok(Sink::Stdout);
        }
        Ok(match Tty::open()? {
            Some(t) => Sink::Tty(t),
            None => Sink::Off,
        })
    }

    /// Writes one report as one sequence, with a single `write(2)`.
    pub fn write(&self, report: &Report) -> io::Result<()> {
        let bytes = report.encode(Terminator::St);
        match self {
            Sink::Tty(t) => t.write_sequence(&bytes),
            Sink::Stdout => tty::write_sequence(io::stdout(), &bytes),
            Sink::Off => Ok(()),
        }
    }
}
