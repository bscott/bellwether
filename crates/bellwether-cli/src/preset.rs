//! Output-watching presets for `run --pty`.
//!
//! A preset says how to read a program's terminal output: a regex whose first
//! capture group is a percentage, and prompt patterns that mean the program is
//! waiting for the user (`blocked`, with `kind=auth` or `question`). Presets come
//! from `$XDG_CONFIG_HOME/bellwether/presets.toml` (by default
//! `~/.config/bellwether/presets.toml`) and from the built-ins below. A user
//! preset replaces a built-in of the same name.
//!
//! ```toml
//! [restic]
//! commands = ["restic"]            # which commands it applies to (default: [name])
//! always = false                   # apply to every command (default: false)
//! progress = '([0-9]+(?:\.[0-9]+)?)%'
//! blocked = [
//!   { pattern = '(?i)enter password[^:]*:\s*$', kind = "auth" },
//!   { pattern = '\[y/N\]\s*$', kind = "question", msg = "Confirm?" },
//! ]
//! ```

use std::path::PathBuf;

use bellwether::Kind;
use regex::bytes::Regex;

/// Presets shipped with bellwether. Parsed with the same loader as the user's
/// file, so they double as a format example.
pub const BUILTIN: &str = r#"
# restic backup / restore / check status lines: "[0:12] 45.20%  ..."
[restic]
progress = '^\[[0-9:]+\]\s+([0-9]{1,3}(?:\.[0-9]+)?)%'
blocked = [
  { pattern = '(?i)enter password[^:]*:\s*$', kind = "auth" },
]

# sudo can prompt from inside any command, so this one always applies.
# Linux: "[sudo] password for alice: ". macOS: "Password:".
[sudo]
always = true
blocked = [
  { pattern = '^\[sudo\] password for [^:]*:\s*$', kind = "auth" },
  { pattern = '^Password:\s*$', kind = "auth" },
]
"#;

/// One preset, compiled.
#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    /// Command basenames this preset applies to.
    pub commands: Vec<String>,
    /// Applies to every command.
    pub always: bool,
    /// First capture group is a percentage.
    pub progress: Option<Regex>,
    pub blocked: Vec<Blocked>,
}

/// A prompt pattern. It is matched against the text of the line the cursor is
/// on, once output pauses.
#[derive(Debug, Clone)]
pub struct Blocked {
    pub pattern: Regex,
    pub kind: Kind,
    /// Report message. Without one, the prompt line itself is used.
    pub msg: Option<String>,
}

/// Where the user's presets file lives.
pub fn user_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("bellwether").join("presets.toml"))
}

/// The built-ins, then the user's file (if it exists) layered on top.
pub fn load_all() -> Result<Vec<Preset>, String> {
    let mut all = parse(BUILTIN, "built-in presets").expect("built-in presets are valid");
    if let Some(path) = user_path() {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let user = parse(&text, &path.display().to_string())?;
                merge(&mut all, user);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(all)
}

/// User presets go first, so they win command matching; same-named built-ins
/// are dropped.
fn merge(all: &mut Vec<Preset>, user: Vec<Preset>) {
    all.retain(|b| !user.iter().any(|u| u.name == b.name));
    let builtins = std::mem::take(all);
    *all = user;
    all.extend(builtins);
}

/// Picks the presets for a command: the named one (`--preset`) or the first
/// whose `commands` matches the command, plus every `always` preset.
pub fn select(
    all: &[Preset],
    explicit: Option<&str>,
    argv: &[String],
) -> Result<Vec<Preset>, String> {
    let main = match explicit {
        Some(name) => Some(all.iter().find(|p| p.name == name).ok_or_else(|| {
            let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
            format!(
                "--preset: no preset named {name:?} (available: {})",
                names.join(", ")
            )
        })?),
        // `always` presets are added below anyway; matching them here would
        // hide the preset for the command they wrap (`sudo restic`).
        None => command_words(argv).find_map(|w| all.iter().find(|p| !p.always && p.applies_to(w))),
    };
    let mut out: Vec<Preset> = main.into_iter().cloned().collect();
    for p in all.iter().filter(|p| p.always) {
        if !out.iter().any(|o| o.name == p.name) {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// Commands that run another command. `sudo -u bob restic backup` is matched
/// as `sudo`, then `restic`.
const WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "env",
    "nice",
    "nohup",
    "time",
    "caffeinate",
    "command",
    "exec",
];

/// The basenames to try against presets: the program, and after a wrapper
/// such as `sudo` or `env`, the program it runs. Options and `VAR=value` words
/// are skipped. Option arguments (`sudo -u bob`) are tried too, which is
/// harmless.
fn command_words(argv: &[String]) -> impl Iterator<Item = &str> {
    let mut done = false;
    argv.iter()
        .map(|a| basename(a))
        .filter(|w| !w.starts_with('-') && !w.contains('='))
        .take_while(move |w| {
            if done {
                return false;
            }
            done = !WRAPPERS.contains(w);
            true
        })
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

impl Preset {
    fn applies_to(&self, word: &str) -> bool {
        self.commands.iter().any(|c| c == word)
    }
}

/// Parses a presets file. `origin` names it in error messages.
pub fn parse(text: &str, origin: &str) -> Result<Vec<Preset>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{origin}: {e}"))?;
    let mut out = Vec::new();
    for (name, value) in table {
        let at = format!("{origin}: [{name}]");
        let toml::Value::Table(t) = value else {
            return Err(format!("{at}: expected a table"));
        };
        out.push(preset(&name, t, &at)?);
    }
    Ok(out)
}

fn preset(name: &str, t: toml::Table, at: &str) -> Result<Preset, String> {
    let mut p = Preset {
        name: name.to_owned(),
        commands: vec![name.to_owned()],
        always: false,
        progress: None,
        blocked: Vec::new(),
    };
    for (key, value) in t {
        match key.as_str() {
            "commands" => p.commands = strings(&value, &format!("{at}.commands"))?,
            "always" => {
                p.always = value
                    .as_bool()
                    .ok_or_else(|| format!("{at}.always: expected true or false"))?
            }
            "progress" => {
                let re = regex(&value, &format!("{at}.progress"))?;
                if re.captures_len() < 2 {
                    return Err(format!(
                        "{at}.progress: needs a capture group for the percentage, like '([0-9]+)%'"
                    ));
                }
                p.progress = Some(re);
            }
            "blocked" => {
                let toml::Value::Array(items) = value else {
                    return Err(format!("{at}.blocked: expected an array of tables"));
                };
                for (i, item) in items.into_iter().enumerate() {
                    p.blocked
                        .push(blocked(item, &format!("{at}.blocked[{i}]"))?);
                }
            }
            other => {
                return Err(format!(
                    "{at}: unknown key {other:?} (expected commands, always, progress, blocked)"
                ));
            }
        }
    }
    Ok(p)
}

fn blocked(item: toml::Value, at: &str) -> Result<Blocked, String> {
    let toml::Value::Table(t) = item else {
        return Err(format!(
            "{at}: expected a table like {{ pattern = '...', kind = \"auth\" }}"
        ));
    };
    let mut pattern = None;
    let mut kind = None;
    let mut msg = None;
    for (key, value) in t {
        match key.as_str() {
            "pattern" => pattern = Some(regex(&value, &format!("{at}.pattern"))?),
            "kind" => {
                let s = value
                    .as_str()
                    .ok_or_else(|| format!("{at}.kind: expected a string"))?;
                kind = Some(s.parse::<Kind>().map_err(|_| {
                    format!("{at}.kind: {s:?} is not one of permission, question, auth")
                })?);
            }
            "msg" => {
                let s = value
                    .as_str()
                    .ok_or_else(|| format!("{at}.msg: expected a string"))?;
                // Same rules as `--msg`: never emit a report that breaks the spec.
                bellwether::Report::builder(bellwether::State::Blocked)
                    .msg(s)
                    .try_build()
                    .map_err(|e| format!("{at}.msg: {e}"))?;
                msg = Some(s.to_owned());
            }
            other => {
                return Err(format!(
                    "{at}: unknown key {other:?} (expected pattern, kind, msg)"
                ));
            }
        }
    }
    Ok(Blocked {
        pattern: pattern.ok_or_else(|| format!("{at}: missing `pattern`"))?,
        kind: kind.ok_or_else(|| format!("{at}: missing `kind`"))?,
        msg,
    })
}

fn strings(value: &toml::Value, at: &str) -> Result<Vec<String>, String> {
    let err = || format!("{at}: expected an array of strings");
    value
        .as_array()
        .ok_or_else(err)?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(err))
        .collect()
}

fn regex(value: &toml::Value, at: &str) -> Result<Regex, String> {
    let s = value
        .as_str()
        .ok_or_else(|| format!("{at}: expected a string (use '...' to avoid escaping)"))?;
    Regex::new(s).map_err(|e| format!("{at}: invalid regex: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn names(p: &[Preset]) -> Vec<&str> {
        p.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn builtins_parse() {
        let all = parse(BUILTIN, "built-in").unwrap();
        assert_eq!(names(&all), ["restic", "sudo"]);
        assert!(all[1].always);
    }

    #[test]
    fn selection() {
        let all = parse(BUILTIN, "built-in").unwrap();
        let sel = |v: &[&str]| names(&select(&all, None, &argv(v)).unwrap()).join(",");
        assert_eq!(sel(&["restic", "backup"]), "restic,sudo");
        assert_eq!(sel(&["/usr/local/bin/restic"]), "restic,sudo");
        assert_eq!(sel(&["sudo", "-E", "restic", "backup"]), "restic,sudo");
        assert_eq!(sel(&["env", "A=1", "restic"]), "restic,sudo");
        assert_eq!(sel(&["make", "restic"]), "sudo");
        assert_eq!(sel(&["ls"]), "sudo");
        let explicit = select(&all, Some("restic"), &argv(&["sh", "-c", "x"])).unwrap();
        assert_eq!(names(&explicit), ["restic", "sudo"]);
        assert!(select(&all, Some("nope"), &argv(&["x"])).is_err());
    }

    #[test]
    fn user_presets_override_builtins() {
        let mut all = parse(BUILTIN, "built-in").unwrap();
        let user = parse(
            "[sudo]\nalways = false\n[mytool]\ncommands = ['mt']\nprogress = '(\\d+)%'\n",
            "user",
        )
        .unwrap();
        merge(&mut all, user);
        assert_eq!(names(&all), ["sudo", "mytool", "restic"]);
        assert!(!all[0].always);
        let sel = select(&all, None, &argv(&["mt"])).unwrap();
        assert_eq!(names(&sel), ["mytool"]);
    }

    #[test]
    fn errors_name_the_problem() {
        let err = |t: &str| parse(t, "f").unwrap_err();
        assert!(err("[a]\nprogress = '('").contains("f: [a].progress: invalid regex"));
        assert!(err("[a]\nprogress = 'x%'").contains("capture group"));
        assert!(err("[a]\nbogus = 1").contains("unknown key \"bogus\""));
        assert!(
            err("[a]\nblocked = [{ pattern = 'x', kind = 'nope' }]")
                .contains("[a].blocked[0].kind")
        );
        assert!(err("[a]\nblocked = [{ kind = 'auth' }]").contains("missing `pattern`"));
        assert!(err("[a]\nblocked = [{ pattern = 'x' }]").contains("missing `kind`"));
        assert!(
            err("[a]\nblocked = [{ pattern = 'x', kind = 'auth', msg = \"a\\u0007\" }]")
                .contains("msg")
        );
        assert!(err("a = 1").contains("expected a table"));
        assert!(err("[a").starts_with("f: "));
    }
}
