//! `bellwether decode`: print the OSC 7501 sequences found on stdin as JSON lines.

use std::io::{self, BufWriter, ErrorKind, Read, Write};
use std::process::ExitCode;

use bellwether::{Event, Message, Scanner};

use crate::json;

pub fn decode() -> ExitCode {
    match decode_stream(io::stdin().lock(), BufWriter::new(io::stdout().lock())) {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (`| head`): not an error.
        Err(e) if e.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bellwether: {e}");
            ExitCode::FAILURE
        }
    }
}

fn decode_stream(mut input: impl Read, mut out: impl Write) -> io::Result<()> {
    let mut scanner = Scanner::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut result = Ok(());
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        scanner.feed(&buf[..n], |ev| {
            if result.is_ok()
                && let Some(line) = line_for(ev)
            {
                result = writeln!(out, "{line}");
            }
        });
        result.as_ref().map_err(clone_err)?;
        // Each line goes out as soon as its chunk is read, so `decode` works
        // live in a pipeline.
        out.flush()?;
    }
    out.flush()
}

fn clone_err(e: &io::Error) -> io::Error {
    io::Error::new(e.kind(), e.to_string())
}

fn line_for(ev: Event<'_>) -> Option<String> {
    Some(match ev {
        Event::Bytes(_) => return None,
        Event::Oversized => "{\"error\":\"sequence longer than 4096 bytes, discarded\"}".to_owned(),
        Event::Sequence { body, message, .. } => match message {
            Message::Report(Ok(r)) => json::report(&r),
            Message::Report(Err(e)) => format!(
                "{{\"error\":{},\"body\":{}}}",
                json::string(&e.to_string()),
                json::string(&String::from_utf8_lossy(body))
            ),
            Message::Query => "{\"query\":true}".to_owned(),
            Message::Reply(Ok(s)) => format!("{{\"reply\":{{{}}}}}", json::support_fields(&s)),
            Message::Reply(Err(e)) => format!("{{\"error\":{}}}", json::string(&e.to_string())),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> String {
        let mut out = Vec::new();
        decode_stream(input, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn lines() {
        let out =
            run(b"x\x1b]7501;state=working:msg=SGk=\x1b\\y\x1b]7501;msg=SGk=\x07\x1b]7501;?\x07");
        assert_eq!(
            out,
            "{\"state\":\"working\",\"msg\":\"Hi\"}\n\
             {\"error\":\"missing state\",\"body\":\"msg=SGk=\"}\n\
             {\"query\":true}\n"
        );
        assert_eq!(run(b"nothing here"), "");
    }
}
