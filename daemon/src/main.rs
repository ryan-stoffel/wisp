use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

const VERSION: &str = concat!("wispd ", env!("CARGO_PKG_VERSION"));
const USAGE: &str = "Usage: wispd [-h | --help] [-V | --version]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
}

fn parse_args(args: &[OsString]) -> Option<Command> {
    let [arg] = args else {
        return None;
    };
    match arg.to_str()? {
        "-h" | "--help" => Some(Command::Help),
        "-V" | "--version" => Some(Command::Version),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    match parse_args(&args) {
        Some(Command::Help) => {
            println!("{VERSION}\nThe wisp host daemon.\n\n{USAGE}");
            ExitCode::SUCCESS
        }
        Some(Command::Version) => {
            println!("{VERSION}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_args};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Option<Command> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse_args(&args)
    }

    #[test]
    fn version_flags_select_version() {
        assert_eq!(parse(&["--version"]), Some(Command::Version));
        assert_eq!(parse(&["-V"]), Some(Command::Help));
    }

    #[test]
    fn help_flags_select_help() {
        assert_eq!(parse(&["--help"]), Some(Command::Help));
        assert_eq!(parse(&["-h"]), Some(Command::Help));
    }

    #[test]
    fn missing_unknown_or_extra_arguments_are_rejected() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&["--bogus"]), None);
        assert_eq!(parse(&["-v"]), None);
        assert_eq!(parse(&["--version", "--help"]), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_argument_is_rejected() {
        use std::os::unix::ffi::OsStringExt;

        let args = [OsString::from_vec(vec![0xff])];
        assert_eq!(parse_args(&args), None);
    }
}
