//! `bellwether`: report program status to the terminal with OSC 7501.

mod decode;
mod json;
mod output;
mod preset;
mod probe;
mod pty;
mod run;
mod sys;
mod watch;

use std::process::ExitCode;
use std::time::Duration;

use bellwether::{App, Id, Kind, State};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand};

/// Report program status to your terminal with OSC 7501 (Program Status Protocol).
#[derive(Parser)]
#[command(
    name = "bellwether",
    version,
    after_help = "\
Reports go to /dev/tty, so they reach the terminal even when stdout is piped.
With no controlling terminal, nothing is written and the exit status is 0.
Set BELLWETHER=0 to turn off all report output.

Spec: https://www.superlogical.com/rex/docs/build/program-status"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write one status report and exit
    #[command(after_help = "\
Examples:
  bellwether emit working --app deploy --msg 'Deploying v2.4.1'
  bellwether emit blocked --kind auth --msg 'Password required'
  bellwether emit working --id us-east --title 'US East' --progress 40
  bellwether emit clear --id us-east     # remove a record and its children
  bellwether emit clear                  # remove every record")]
    Emit(EmitArgs),

    /// Run a command, reporting `working` while it runs and the result after
    #[command(after_help = "\
The final report is:
  done    the command exited 0
  error   it exited nonzero, or was killed by a signal other than those below
  idle    it was interrupted (SIGINT) or terminated (SIGTERM, SIGHUP)

Ctrl-C and Ctrl-\\ reach the command directly from the terminal, so they are not
forwarded again, and a SIGINT or SIGQUIT sent to bellwether's pid alone is
ignored: signal the process group, or use SIGTERM. (With --pty the command has
its own terminal, so SIGINT and SIGQUIT sent to bellwether are passed on.)
SIGTERM and SIGHUP sent to bellwether are forwarded. bellwether exits with the
command's status. If the command was killed by a signal, bellwether re-raises
that signal on itself.

With --pty the command runs in a pseudoterminal and bellwether watches its
output. A preset's progress pattern sets the percentage, and its prompt
patterns report `blocked` (kind auth or question) until you press Enter. Presets
come from ~/.config/bellwether/presets.toml (or $XDG_CONFIG_HOME) and the
built-ins restic, git, apt, dnf, rsync and cargo, plus sudo and ssh, which are
always on. The preset is picked by command name, or with --preset. If the
command sends its own OSC 7501 report for the record bellwether is using,
bellwether moves to the child record `bellwether` beneath it.

Nested runs: bellwether sets BELLWETHER_PARENT_ID for the command, and a
`bellwether run` that finds it reports on a child record (parent/app).

Examples:
  bellwether run --app restic -- restic backup ~/Documents
  bellwether run --title 'Nightly backup' -- ./backup.sh
  bellwether run --pty -- restic backup ~/Documents
  bellwether run --pty --preset restic -- ./backup.sh")]
    Run(RunArgs),

    /// Ask the terminal whether it supports OSC 7501
    #[command(after_help = "\
Puts the terminal in raw mode briefly, sends the OSC 7501 query followed by a
primary device attributes request (CSI c), and waits for the replies. Keys typed
while the probe waits may be consumed.

Exit status: 0 supported, 1 unsupported or timed out, 3 no controlling terminal.")]
    Probe(ProbeArgs),

    /// Print the OSC 7501 sequences found on stdin as JSON lines
    #[command(after_help = "\
Prints one JSON object per line for each report found, and one with an \"error\"
key for each sequence that was discarded. Other input is ignored.

Example:
  printf '\\e]7501;state=working:msg=SGk=\\e\\\\' | bellwether decode")]
    Decode,
}

#[derive(Args)]
struct OutputArgs {
    /// Write reports to stdout instead of /dev/tty, even without a terminal
    #[arg(long)]
    stdout: bool,
}

const STATES: [&str; 6] = ["idle", "working", "done", "blocked", "error", "clear"];
const KINDS: [&str; 3] = ["permission", "question", "auth"];

fn state_parser() -> impl TypedValueParser<Value = State> {
    PossibleValuesParser::new(STATES).map(|s| s.parse::<State>().expect("listed value"))
}

fn kind_parser() -> impl TypedValueParser<Value = Kind> {
    PossibleValuesParser::new(KINDS).map(|s| s.parse::<Kind>().expect("listed value"))
}

#[derive(Args)]
struct EmitArgs {
    /// The state to report. `clear` removes the record and everything beneath it
    #[arg(value_parser = state_parser())]
    state: State,

    /// One line saying what is happening (≤ 2048 bytes, no control characters)
    #[arg(long)]
    msg: Option<String>,

    /// Short label for the record (≤ 192 bytes)
    #[arg(long)]
    title: Option<String>,

    /// Stable program identifier, [A-Za-z0-9_.+-]{1,32}
    #[arg(long)]
    app: Option<App>,

    /// Record id, such as `build` or `build/test`. Omit for the root record
    #[arg(long)]
    id: Option<Id>,

    /// What a blocked program is waiting for (blocked only)
    #[arg(long, value_parser = kind_parser())]
    kind: Option<Kind>,

    /// Percent complete, 0-100 (working and blocked only)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(0..=100))]
    progress: Option<u8>,

    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args)]
struct RunArgs {
    /// Program identifier [default: the command's basename]
    #[arg(long)]
    app: Option<App>,

    /// Message while running [default: the command line]
    #[arg(long)]
    msg: Option<String>,

    /// Message when the command succeeds [default: e.g. "Finished in 12s"]
    #[arg(long)]
    done_msg: Option<String>,

    /// Message when the command fails [default: e.g. "Exited 1 after 12s"]
    #[arg(long)]
    error_msg: Option<String>,

    /// Short label for the record, sent with every report (≤ 192 bytes)
    #[arg(long)]
    title: Option<String>,

    /// Record id to report on. Omit for the root record. When nested inside
    /// another `bellwether run`, it is relative to that run's record
    #[arg(long)]
    id: Option<Id>,

    /// Run the command in a pseudoterminal and watch its output for progress
    /// and prompts
    #[arg(long)]
    pty: bool,

    /// Preset to watch the output with [default: chosen by command name]
    #[arg(long, value_name = "NAME", requires = "pty")]
    preset: Option<String>,

    #[command(flatten)]
    output: OutputArgs,

    /// The command to run, after `--`
    #[arg(last = true, required = true, value_name = "CMD")]
    command: Vec<String>,
}

#[derive(Args)]
struct ProbeArgs {
    /// How long to wait for the terminal, e.g. 200ms, 1s, 0.5s
    #[arg(long, default_value = "200ms", value_parser = parse_duration)]
    timeout: Duration,

    /// Print the result as JSON
    #[arg(long)]
    json: bool,
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else {
        (s, 1e-3)
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration {s:?} (try 200ms or 1s)"))?;
    Duration::try_from_secs_f64(v * scale).map_err(|e| format!("invalid duration {s:?}: {e}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Emit(args) => emit(args),
        Command::Run(args) => run::run(args),
        Command::Probe(args) => probe::probe(args),
        Command::Decode => decode::decode(),
    }
}

fn emit(args: EmitArgs) -> ExitCode {
    let mut b = bellwether::Report::builder(args.state).maybe_id(args.id);
    if let Some(k) = args.kind {
        b = b.kind(k);
    }
    if let Some(p) = args.progress {
        b = b.progress(bellwether::Progress::new(p).expect("clap checked the range"));
    }
    if let Some(a) = args.app {
        b = b.app(a);
    }
    if let Some(t) = args.title {
        b = b.title(t);
    }
    if let Some(m) = args.msg {
        b = b.msg(m);
    }
    let report = match b.try_build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bellwether: {e}");
            return ExitCode::from(2);
        }
    };
    let sink = match output::Sink::open(args.output.stdout) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bellwether: cannot open /dev/tty: {e}");
            return ExitCode::FAILURE;
        }
    };
    match sink.write(&report) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bellwether: write failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("200ms"), Ok(Duration::from_millis(200)));
        assert_eq!(parse_duration("1s"), Ok(Duration::from_secs(1)));
        assert_eq!(parse_duration("0.5s"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("150"), Ok(Duration::from_millis(150)));
        assert!(parse_duration("fast").is_err());
        assert!(parse_duration("-1s").is_err());
    }

    #[test]
    fn clap_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
