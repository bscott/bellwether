//! `bellwether probe`: feature detection with the DA1 race.

use std::process::ExitCode;

use bellwether::Detection;
use bellwether::tty::Tty;

use crate::ProbeArgs;
use crate::json;
use crate::sys::Hold;

pub fn probe(args: ProbeArgs) -> ExitCode {
    let tty = match Tty::open() {
        Ok(Some(t)) => t,
        Ok(None) => return no_tty(&args, "no controlling terminal"),
        Err(e) => return no_tty(&args, &format!("cannot open /dev/tty: {e}")),
    };

    let result = {
        // Hold INT/QUIT/TERM/HUP while the terminal is raw. A signal that arrives
        // meanwhile is delivered after the termios guard inside `probe` has run
        // (on return or unwind), so the terminal is never left raw. Raw mode
        // clears ISIG, so Ctrl-C typed now arrives as a byte, not a signal.
        let _hold = Hold::new();
        tty.probe(args.timeout)
    };

    match result {
        Ok(Detection::Supported(s)) => {
            if args.json {
                println!("{{\"supported\":true,{}}}", json::support_fields(&s));
            } else {
                println!("supported");
                println!("states: {}", s.states.join(" "));
                println!("kinds:  {}", s.kinds.join(" "));
            }
            ExitCode::SUCCESS
        }
        Ok(_) => {
            if args.json {
                println!("{{\"supported\":false}}");
            } else {
                println!("unsupported");
            }
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("bellwether: probe failed: {e}");
            if args.json {
                println!(
                    "{{\"supported\":false,\"error\":{}}}",
                    json::string(&e.to_string())
                );
            }
            ExitCode::from(1)
        }
    }
}

fn no_tty(args: &ProbeArgs, why: &str) -> ExitCode {
    if args.json {
        println!("{{\"supported\":false,\"error\":{}}}", json::string(why));
    } else {
        eprintln!("bellwether: {why}");
    }
    ExitCode::from(3)
}
