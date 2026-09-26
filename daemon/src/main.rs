use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info, warn};
use wispd::VERSION;
use wispd::attach::{
    self, DEFAULT_CONNECT_TIMEOUT, EXIT_UNAVAILABLE, MAX_CONNECT_TIMEOUT, Options, report,
};
use wispd::launch_agent::LaunchAgent;
use wispd::logging::{self, DEFAULT_LOG_LEVEL, LOG_LEVEL_ENV, LogFilter};
use wispd::paths::{DATA_DIR_ENV, DataDir};
use wispd::server::{self, Config, EXIT_ALREADY_RUNNING, Server, Shutdown, StartError};
use wispd::service::{self, DEFAULT_LABEL, SERVICE_LABEL_ENV};

#[derive(Debug, Parser)]
#[command(name = "wispd", version = VERSION, about = "The wisp host daemon.")]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the editor on this user's socket until SIGTERM or SIGINT.
    Serve(ServeArgs),
    /// Connect stdin and stdout to wispd's socket, starting wispd if it isn't running.
    Attach(AttachArgs),
    /// Manage wispd's per-user `LaunchAgent`.
    Service(ServiceArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// The data folder [default: ~/Library/Application Support/wisp]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// off, error, warn, info, debug, or trace, or a list such as wispd=debug,warn
    #[arg(long, value_name = "LEVEL", env = LOG_LEVEL_ENV, default_value = DEFAULT_LOG_LEVEL)]
    log_level: LogFilter,
}

#[derive(Debug, Args)]
struct AttachArgs {
    /// The data folder [default: ~/Library/Application Support/wisp]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// How long to wait for wispd to accept a connection, including starting it [default: 10]
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    connect_timeout: Option<Duration>,
}

fn parse_seconds(text: &str) -> Result<Duration, String> {
    text.parse::<f64>()
        .ok()
        .filter(|seconds| *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .filter(|duration| *duration <= MAX_CONNECT_TIMEOUT)
        .ok_or_else(|| {
            format!(
                "{text:?} is not a number of seconds greater than 0 and at most {}",
                MAX_CONNECT_TIMEOUT.as_secs()
            )
        })
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Install or update the `LaunchAgent`, then start or restart it.
    Install(ServiceOptions),
    /// Stop the `LaunchAgent` if it is running, and remove it.
    Uninstall(ServiceOptions),
    /// Report whether the `LaunchAgent` is installed, loaded, and running.
    Status(ServiceOptions),
}

#[derive(Debug, Args)]
struct ServiceOptions {
    /// The data folder [default: ~/Library/Application Support/wisp]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// Override the `LaunchAgent`'s label. For tests: a real install never needs this.
    #[arg(long, value_name = "LABEL", env = SERVICE_LABEL_ENV, default_value = DEFAULT_LABEL, hide = true)]
    label: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => serve(&args),
        Command::Attach(args) => attach(&args),
        Command::Service(args) => match args.command {
            ServiceCommand::Install(options) => service_install(&options),
            ServiceCommand::Uninstall(options) => service_uninstall(&options),
            ServiceCommand::Status(options) => service_status(&options),
        },
    }
}

/// Runs `attach` and exits, with `std::process::exit`: the thread that reads stdin blocks until
/// input arrives, and would keep the runtime from shutting down (0007).
fn attach(args: &AttachArgs) -> ! {
    let data_dir = match DataDir::resolve(args.data_dir.as_deref()) {
        Ok(data_dir) => data_dir,
        Err(error) => unavailable(&format!("could not find the data folder: {error}")),
    };
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => unavailable(&format!("could not find wispd's own executable: {error}")),
    };
    let options = Options {
        program,
        connect_timeout: args.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT),
        launch_agent: LaunchAgent::installed_for(&data_dir),
    };
    // Starting wispd happens here, before the runtime exists.
    let stream = match attach::connect(&data_dir, &options) {
        Ok(stream) => stream,
        Err(error) => unavailable(&error),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => failed(&format!("could not start the runtime: {error}")),
    };
    let code = runtime.block_on(async {
        let stream = match stream
            .set_nonblocking(true)
            .and_then(|()| UnixStream::from_std(stream))
        {
            Ok(stream) => stream,
            Err(error) => failed(&format!("could not use the connection: {error}")),
        };
        // Handling SIGHUP, rather than leaving its default, also overrides an ignored SIGHUP
        // inherited from a parent such as nohup, so a dropped SSH session always ends attach.
        let mut hangup = match signal(SignalKind::hangup()) {
            Ok(hangup) => hangup,
            Err(error) => failed(&format!("could not catch SIGHUP: {error}")),
        };
        tokio::select! {
            bridged = attach::bridge(tokio::io::stdin(), tokio::io::stdout(), stream) => {
                match bridged {
                    Ok(()) => 0,
                    Err(error) => {
                        report(format_args!("the connection failed: {error}"));
                        1
                    }
                }
            }
            _ = hangup.recv() => 0,
        }
    });
    std::process::exit(code)
}

fn unavailable(message: &dyn std::fmt::Display) -> ! {
    report(message);
    std::process::exit(EXIT_UNAVAILABLE.into())
}

fn failed(message: &str) -> ! {
    report(message);
    std::process::exit(1)
}

fn serve(args: &ServeArgs) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("could not start the runtime: {error}")),
    };
    runtime.block_on(async {
        // Signals are caught before the socket exists, so a SIGTERM during startup still ends
        // in a clean shutdown.
        let shutdown = Shutdown::default();
        if let Err(error) = catch_signals(shutdown.clone()) {
            return fail(&format!("could not catch signals: {error}"));
        }
        let data_dir = match DataDir::resolve(args.data_dir.as_deref()) {
            Ok(data_dir) => data_dir,
            Err(error) => return fail(&format!("could not find the data folder: {error}")),
        };
        if let Err(error) = server::prepare_data_dir(data_dir.root()) {
            return fail(&error.to_string());
        }
        if let Err(error) = logging::init(&data_dir.log_file(), &args.log_level) {
            return fail(&format!(
                "could not log to {}: {error}",
                data_dir.log_file().display()
            ));
        }
        info!(log_level = %args.log_level, "starting");
        // Every path wispd uses is absolute by now. Leaving the directory it was started in
        // keeps a folder, or the volume it is on, from staying busy for as long as wispd runs.
        if let Err(error) = std::env::set_current_dir("/") {
            warn!(%error, "could not change to the root folder");
        }
        let server = match Server::start(Config::new(data_dir)) {
            Ok(server) => server,
            Err(error @ StartError::AlreadyRunning { .. }) => {
                warn!(%error, "not starting");
                eprintln!("wispd: {error}");
                return ExitCode::from(EXIT_ALREADY_RUNNING);
            }
            Err(error) => {
                error!(%error, "could not start");
                return fail(&error.to_string());
            }
        };
        match server.run(shutdown).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                error!(%error, "stopped with an error");
                fail(&error.to_string())
            }
        }
    })
}

fn service_install(options: &ServiceOptions) -> ExitCode {
    let data_dir = match resolve_data_dir(options) {
        Ok(data_dir) => data_dir,
        Err(code) => return code,
    };
    match service::install(&options.label, &data_dir) {
        Ok(service::InstallOutcome::Installed) => {
            println!("installed and started {}", options.label);
            ExitCode::SUCCESS
        }
        Ok(service::InstallOutcome::Reinstalled) => {
            println!("updated the plist and restarted {}", options.label);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn service_uninstall(options: &ServiceOptions) -> ExitCode {
    match service::uninstall(&options.label) {
        Ok(service::UninstallOutcome::Removed) => {
            println!("uninstalled {}", options.label);
            ExitCode::SUCCESS
        }
        Ok(service::UninstallOutcome::NotInstalled) => {
            println!("{} was not installed", options.label);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn service_status(options: &ServiceOptions) -> ExitCode {
    let data_dir = match resolve_data_dir(options) {
        Ok(data_dir) => data_dir,
        Err(code) => return code,
    };
    match service::status(&options.label, &data_dir) {
        Ok(status) => {
            println!("label: {}", status.label);
            println!("plist: {}", status.plist_path.display());
            println!("installed: {}", status.installed);
            println!("loaded: {}", status.launchd.loaded());
            println!("running: {}", status.launchd.running());
            println!(
                "pid: {}",
                status
                    .launchd
                    .pid()
                    .map_or_else(|| "-".to_owned(), |pid| pid.to_string())
            );
            println!("answers initialize: {}", status.answers_initialize);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn resolve_data_dir(options: &ServiceOptions) -> Result<DataDir, ExitCode> {
    DataDir::resolve(options.data_dir.as_deref())
        .map_err(|error| fail(&format!("could not find the data folder: {error}")))
}

fn catch_signals(shutdown: Shutdown) -> io::Result<()> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        loop {
            let name = tokio::select! {
                _ = terminate.recv() => "SIGTERM",
                _ = interrupt.recv() => "SIGINT",
            };
            info!(signal = name, "received a signal to stop");
            shutdown.trigger();
        }
    });
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("wispd: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::{CommandFactory, Parser};

    use super::{Cli, Command};

    #[test]
    fn the_command_line_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn attach_takes_a_timeout_in_seconds_up_to_a_day() {
        let timeout = |seconds: &str| {
            Cli::try_parse_from(["wispd", "attach", "--connect-timeout", seconds]).map(|cli| {
                let Command::Attach(args) = cli.command else {
                    panic!("expected attach, got {:?}", cli.command);
                };
                args.connect_timeout
            })
        };
        assert_eq!(timeout("2.5").unwrap(), Some(Duration::from_millis(2500)));
        assert_eq!(timeout("86400").unwrap(), Some(Duration::from_hours(24)));
        for bad in ["0", "-1", "soon", "NaN", "inf", "1e19", "86401"] {
            assert!(timeout(bad).is_err(), "{bad}");
        }
    }
}
