//! Use case 1: a long-running program reports its own status.
//!
//! A deploy script shows progress while it works, `blocked` while it waits for
//! a confirmation, and `done` or `error` at the end. Terminals that support
//! OSC 7501 (Rex, Herdr) show this in tab headers and sidebars; others ignore
//! it.
//!
//! Run it in a terminal: `cargo run -p bellwether --features tty --example report_status`

use std::io::{self, BufRead, Write};
use std::thread::sleep;
use std::time::Duration;

use bellwether::tty::Tty;
use bellwether::{App, Kind, Progress, Report, State, Terminator};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `None` when there is no controlling terminal (cron, CI): report nothing.
    let tty = Tty::open()?;
    let app = App::new("deploy")?;
    let report = |r: Report| -> io::Result<()> {
        match &tty {
            // One write(2) per report, to /dev/tty, so it works even when
            // stdout is piped to a log file.
            Some(t) => t.write_sequence(&r.encode(Terminator::St)),
            None => Ok(()),
        }
    };

    for step in 0..=4u8 {
        // `build_lossy` never fails: it trims an over-long message and strips
        // control characters instead.
        report(
            Report::builder(State::Working)
                .app(app.clone())
                .progress(Progress::new(step * 20).expect("0..=100"))
                .msg(format!("Uploading release, step {} of 5", step + 1))
                .build_lossy(),
        )?;
        sleep(Duration::from_millis(400));
    }

    report(
        Report::builder(State::Blocked)
            .app(app.clone())
            .kind(Kind::Question)
            .msg("Promote to production? [y/N]")
            .build_lossy(),
    )?;
    print!("Promote to production? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;

    let done = if answer.trim().eq_ignore_ascii_case("y") {
        Report::builder(State::Done).msg("Deployed v2.4.1")
    } else {
        Report::builder(State::Error).msg("Promotion declined")
    };
    report(done.app(app).build_lossy())?;
    Ok(())
}
