//! `bellwether status`: show the records that the OSC 7501 reports on stdin
//! add up to, as a table.
//!
//! Reports are applied with the library's [`RecordStore`], so replacing,
//! subtree `clear` and `app` inheritance follow the spec. When stdout is a
//! terminal the table is redrawn in place as reports arrive. Otherwise it is
//! printed once, at the end of input. Either way the final table shows each
//! record's last report: the lifetime rule that drops `working` records when a
//! program exits is not applied, because there is no program to watch, only a
//! stream.

use std::io::{self, BufWriter, ErrorKind, IsTerminal, Read, Write};
use std::process::ExitCode;

use bellwether::{Event, Message, RecordStore, Scanner};

pub fn status() -> ExitCode {
    let stdout = io::stdout();
    let live = stdout.is_terminal();
    let size = live.then(term_size).flatten();
    match show(
        io::stdin().lock(),
        BufWriter::new(stdout.lock()),
        live,
        size,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bellwether: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The terminal's size on stdout, as (rows, columns).
type Size = (usize, usize);

fn term_size() -> Option<Size> {
    let ws = rustix::termios::tcgetwinsize(io::stdout()).ok()?;
    (ws.ws_row > 0 && ws.ws_col > 0).then_some((usize::from(ws.ws_row), usize::from(ws.ws_col)))
}

/// Reads `input` to the end. `live` redraws the table after every change;
/// otherwise it is written once at the end. With a `size`, the table is cut to
/// fit, so redrawing in place never scrolls or wraps.
fn show(
    mut input: impl Read,
    mut out: impl Write,
    live: bool,
    size: Option<Size>,
) -> io::Result<()> {
    let mut scanner = Scanner::new();
    let mut store = RecordStore::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut drawn = 0;
    if live {
        drawn = draw(&mut out, &store, size, drawn)?;
    }
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let mut changed = false;
        scanner.feed(&buf[..n], |ev| {
            if let Event::Sequence {
                message: Message::Report(Ok(r)),
                ..
            } = ev
            {
                changed |= !store.apply(r).is_empty();
            }
        });
        if live && changed {
            drawn = draw(&mut out, &store, size, drawn)?;
        }
    }
    if !live {
        draw(&mut out, &store, None, 0)?;
    }
    out.flush()
}

/// Writes the table, first erasing the `previous` lines drawn before. Returns
/// how many lines it drew.
fn draw(
    out: &mut impl Write,
    store: &RecordStore,
    size: Option<Size>,
    previous: usize,
) -> io::Result<usize> {
    if previous > 0 {
        // Up to the first line of the old table, then erase to the end.
        write!(out, "\x1b[{previous}A\r\x1b[J")?;
    }
    let mut lines = table(store);
    if let Some((rows, cols)) = size {
        // One row stays free for the cursor after the last line.
        let max = rows.saturating_sub(1).max(2);
        if lines.len() > max {
            let hidden = lines.len() - (max - 1);
            lines.truncate(max - 1);
            lines.push(format!("… {hidden} more"));
        }
        for line in &mut lines {
            *line = truncate(line, cols.saturating_sub(1));
        }
    }
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    Ok(lines.len())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

const HEADER: [&str; 5] = ["RECORD", "STATE", "PROGRESS", "APP", "STATUS"];

/// The records as aligned lines: a header, then the root record and the rest
/// in id order. The last column isn't padded.
fn table(store: &RecordStore) -> Vec<String> {
    if store.is_empty() {
        return vec!["No records.".to_owned()];
    }
    let mut rows = vec![HEADER.map(str::to_owned)];
    for (id, rec) in store.iter() {
        let state = match rec.kind() {
            Some(k) => format!("{} ({k})", rec.state()),
            None => rec.state().to_string(),
        };
        let text = match (rec.title(), rec.msg()) {
            (Some(t), Some(m)) => format!("{t}: {m}"),
            (Some(t), None) => t.to_owned(),
            (None, Some(m)) => m.to_owned(),
            (None, None) => String::new(),
        };
        rows.push([
            id.map_or_else(|| "(root)".to_owned(), |i| i.to_string()),
            state,
            rec.progress().map(|p| format!("{p}%")).unwrap_or_default(),
            store
                .effective_app(id)
                .map(|a| a.to_string())
                .unwrap_or_default(),
            text,
        ]);
    }
    let mut widths = [0; 5];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    line.push_str(cell);
                    let pad = widths[i] - cell.chars().count() + 2;
                    line.extend(std::iter::repeat_n(' ', pad));
                }
            }
            line.trim_end().to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8], live: bool, size: Option<Size>) -> String {
        let mut out = Vec::new();
        show(input, &mut out, live, size).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn final_table() {
        let input = b"noise\
            \x1b]7501;state=working:app=deploy:progress=40:msg=VXBsb2FkaW5n\x1b\\\
            \x1b]7501;state=blocked:kind=auth:id=db\x07\
            \x1b]7501;state=done:id=web/eu:title=RVU=:msg=RGVwbG95ZWQ=\x07\
            \x1b]7501;state=working:id=gone\x07\
            \x1b]7501;state=clear:id=gone\x07\
            \x1b]7501;msg=bWlzc2luZyBzdGF0ZQ==\x07";
        assert_eq!(
            run(input, false, None),
            "RECORD  STATE           PROGRESS  APP     STATUS\n\
             (root)  working         40%       deploy  Uploading\n\
             db      blocked (auth)            deploy\n\
             web/eu  done                      deploy  EU: Deployed\n"
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(run(b"just text", false, None), "No records.\n");
    }

    #[test]
    fn live_redraws_in_place() {
        let input = b"\x1b]7501;state=working:msg=YWJjZGVmZ2hpams=\x07";
        let out = run(input, true, Some((24, 30)));
        assert_eq!(
            out,
            "No records.\n\
             \x1b[1A\r\x1b[J\
             RECORD  STATE    PROGRESS  A…\n\
             (root)  working             …\n"
        );
    }

    #[test]
    fn live_table_fits_the_terminal_height() {
        let mut input = Vec::new();
        for i in 0..10 {
            input.extend_from_slice(format!("\x1b]7501;state=working:id=r{i}\x07").as_bytes());
        }
        let out = run(&input, true, Some((5, 80)));
        let last = out.rsplit("\x1b[J").next().unwrap();
        assert_eq!(
            last,
            "RECORD  STATE    PROGRESS  APP  STATUS\nr0      working\nr1      working\n… 8 more\n"
        );
    }
}
