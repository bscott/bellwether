//! Use case 2: a tool that runs other programs tracks their status.
//!
//! A CI log viewer, a test runner or a terminal multiplexer reads a child's
//! output. `Scanner` finds the OSC 7501 reports in the byte stream and passes
//! every other byte through untouched; `RecordStore` keeps the current status
//! of each record the way a terminal would.
//!
//! This one reads stdin, copies it to stdout and prints each status change to
//! stderr:
//!
//! `bellwether run --stdout -- make test | cargo run -p bellwether --example track_status`

use std::io::{self, Read, Write};

use bellwether::{Change, Event, Message, RecordStore, Scanner};

fn main() -> io::Result<()> {
    let mut scanner = Scanner::new();
    let mut store = RecordStore::new();
    let mut stdout = io::stdout().lock();
    let mut buf = [0u8; 8192];

    let mut handle = |event: Event<'_>, out: &mut dyn Write| -> io::Result<()> {
        match event {
            // Everything that isn't a complete OSC 7501 sequence.
            Event::Bytes(b) => out.write_all(b)?,
            Event::Sequence {
                message: Message::Report(Ok(report)),
                ..
            } => {
                for change in store.apply(report) {
                    match change {
                        Change::Set(id) => {
                            let rec = store.get(id.as_ref()).expect("just set");
                            let name = id.as_ref().map_or("(root)", |i| i.as_str());
                            let app = store.effective_app(id.as_ref());
                            eprintln!(
                                "[{name}] {} {}{}",
                                rec.state().as_str(),
                                app.map_or("", |a| a.as_str()),
                                rec.progress()
                                    .map_or(String::new(), |p| format!(" {}%", p.get())),
                            );
                        }
                        Change::Removed { id, reason } => {
                            eprintln!(
                                "[{}] removed ({reason:?})",
                                id.as_ref().map_or("(root)", |i| i.as_str())
                            );
                        }
                    }
                }
            }
            // A report the spec says to discard, a query, or a reply.
            Event::Sequence { .. } | Event::Oversized => {}
        }
        Ok(())
    };

    let mut stdin = io::stdin().lock();
    loop {
        let n = stdin.read(&mut buf)?;
        let mut result = Ok(());
        if n == 0 {
            scanner.flush(|e| {
                if result.is_ok() {
                    result = handle(e, &mut stdout);
                }
            });
            result?;
            break;
        }
        scanner.feed(&buf[..n], |e| {
            if result.is_ok() {
                result = handle(e, &mut stdout);
            }
        });
        result?;
    }
    stdout.flush()
}
