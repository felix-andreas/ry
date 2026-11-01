//! Property and fuzz coverage of argument parsing for `ry run`, `ry R` and `ry repl`: whatever
//! follows the script reaches it verbatim, options resolve to the documented runtime, and no
//! argument vector panics the parser. `FUZZ_ITERS` scales the budgets; runs are deterministic.

use clap::Parser;
use ry::command_line::{Cli, Keybindings, Mode, Runtime, Session};
use std::path::PathBuf;
use syntax::testing::{SplitMix64, fuzz_iterations};

/// Tokens that look like options of ry's, of R's, or of nothing, plus ordinary words.
const TOKENS: &[&str] = &[
    "-i",
    "--interactive",
    "--keybindings",
    "vi",
    "emacs",
    "--runtime",
    "ry",
    "gnu-r",
    "--no-load",
    "-f",
    "--file",
    "--",
    "-",
    "--help",
    "-h",
    "--version",
    "--vanilla",
    "--args",
    "-e",
    "script.R",
    "other.R",
    "a b",
    "x=1",
    "--x=1",
    "",
    "ü",
    "R",
    "run",
    "repl",
];

fn token(random: &mut SplitMix64) -> String {
    TOKENS[random.below(TOKENS.len())].to_owned()
}

fn session(arguments: &[String]) -> Result<Option<Session>, clap::Error> {
    Cli::try_parse_from(std::iter::once("ry".to_owned()).chain(arguments.iter().cloned()))
        .map(|cli| cli.command.session(None))
}

#[test]
fn everything_after_the_script_reaches_it_verbatim() {
    let mut random = SplitMix64(0x5eed_0001);
    for _ in 0..fuzz_iterations(2000) {
        let trailing: Vec<String> = (0..random.below(6)).map(|_| token(&mut random)).collect();
        for (command, runtime) in [("run", Runtime::Ry), ("R", Runtime::GnuR)] {
            let mut arguments = vec![command.to_owned(), "script.R".to_owned()];
            arguments.extend(trailing.iter().cloned());
            let parsed = session(&arguments)
                .unwrap_or_else(|error| panic!("{arguments:?} must parse: {error}"));
            assert_eq!(
                parsed,
                Some(Session {
                    runtime,
                    mode: Mode::Script {
                        file: PathBuf::from("script.R"),
                        arguments: trailing.clone(),
                    },
                }),
                "{arguments:?}"
            );
        }
    }
}

/// The options a session accepts before its script, and what the model expects of them.
#[derive(Default, Debug)]
struct Options {
    interactive: bool,
    keybindings: Option<Keybindings>,
    runtime: Option<Runtime>,
    no_load: bool,
    file: bool,
}

#[test]
fn options_before_the_script_resolve_as_documented() {
    let mut random = SplitMix64(0x5eed_0002);
    for _ in 0..fuzz_iterations(3000) {
        let command = ["run", "R", "repl"][random.below(3)];
        let mut options = Options::default();
        let mut arguments = vec![command.to_owned()];
        for _ in 0..random.below(4) {
            match random.below(4) {
                0 if command != "repl" && !options.interactive => {
                    options.interactive = true;
                    arguments.push(["-i", "--interactive"][random.below(2)].to_owned());
                    arguments.push("script.R".to_owned());
                }
                1 if options.keybindings.is_none() => {
                    let keybindings = [Keybindings::Emacs, Keybindings::Vi][random.below(2)];
                    options.keybindings = Some(keybindings);
                    arguments.push("--keybindings".to_owned());
                    arguments.push(
                        if keybindings == Keybindings::Vi {
                            "vi"
                        } else {
                            "emacs"
                        }
                        .to_owned(),
                    );
                }
                2 if command != "R" && options.runtime.is_none() => {
                    let runtime = [Runtime::Ry, Runtime::GnuR][random.below(2)];
                    options.runtime = Some(runtime);
                    arguments.push("--runtime".to_owned());
                    arguments.push(
                        if runtime == Runtime::Ry {
                            "ry"
                        } else {
                            "gnu-r"
                        }
                        .to_owned(),
                    );
                }
                3 if !options.no_load => {
                    options.no_load = true;
                    arguments.push("--no-load".to_owned());
                }
                _ => {}
            }
        }
        options.file = options.interactive || random.chance(1, 2);
        if options.file {
            if command == "repl" {
                arguments.push("-f".to_owned());
            }
            if !options.interactive {
                arguments.push("script.R".to_owned());
            }
        }
        // A console option beside a script would be ignored, so it is a usage error.
        let script = options.file && command != "repl" && !options.interactive;
        if script && (options.keybindings.is_some() || options.no_load) {
            assert!(
                session(&arguments).is_err(),
                "{arguments:?} must be refused"
            );
            continue;
        }
        let parsed = session(&arguments)
            .unwrap_or_else(|error| panic!("{arguments:?} must parse: {error}"))
            .expect("a session command");
        let default = if command == "run" {
            Runtime::Ry
        } else {
            Runtime::GnuR
        };
        let runtime = if command == "R" {
            Runtime::GnuR
        } else {
            options.runtime.unwrap_or(default)
        };
        let file = options.file.then(|| PathBuf::from("script.R"));
        let mode = match file {
            Some(file) if command != "repl" && !options.interactive => Mode::Script {
                file,
                arguments: Vec::new(),
            },
            file => Mode::Console {
                file,
                keybindings: options.keybindings.unwrap_or_default(),
                load_package: !options.no_load,
            },
        };
        assert_eq!(parsed, Session { runtime, mode }, "{arguments:?}");
    }
}

#[test]
fn the_runtime_variable_yields_to_the_flag_and_never_reaches_r() {
    let parse = |arguments: &[&str], variable: Option<&str>| {
        Cli::try_parse_from(std::iter::once(&"ry").chain(arguments))
            .expect("parses")
            .command
            .session(variable)
            .expect("a session")
            .runtime
    };
    for variable in [None, Some("ry"), Some("gnu-r"), Some("GNU-R"), Some("")] {
        let named = match variable {
            Some("ry") => Some(Runtime::Ry),
            Some("gnu-r") => Some(Runtime::GnuR),
            _ => None,
        };
        assert_eq!(parse(&["run"], variable), named.unwrap_or(Runtime::Ry));
        assert_eq!(parse(&["repl"], variable), named.unwrap_or(Runtime::GnuR));
        assert_eq!(parse(&["R"], variable), Runtime::GnuR);
        assert_eq!(
            parse(&["run", "--runtime", "gnu-r"], variable),
            Runtime::GnuR
        );
        assert_eq!(parse(&["repl", "--runtime", "ry"], variable), Runtime::Ry);
    }
}

#[test]
fn arbitrary_argument_vectors_never_panic_and_keep_the_session_invariants() {
    let mut random = SplitMix64(0x5eed_0003);
    for _ in 0..fuzz_iterations(5000) {
        let arguments: Vec<String> = (0..random.below(7)).map(|_| token(&mut random)).collect();
        let command = arguments.first().cloned();
        let Ok(Some(session)) = session(&arguments) else {
            continue;
        };
        if command.as_deref() == Some("R") {
            assert_eq!(session.runtime, Runtime::GnuR, "{arguments:?}");
        }
        match session.mode {
            Mode::Script {
                file,
                arguments: trailing,
            } => {
                assert_ne!(command.as_deref(), Some("repl"), "{arguments:?}");
                // An option's value can repeat the script's text, so the script is found from
                // the end: it is the argument just before the ones it receives.
                assert!(arguments.ends_with(&trailing), "{arguments:?}");
                let position = arguments.len() - trailing.len() - 1;
                assert!(position >= 1, "{arguments:?}");
                assert_eq!(
                    file.as_os_str(),
                    arguments[position].as_str(),
                    "{arguments:?}"
                );
            }
            Mode::Console { .. } => {}
        }
    }
}
