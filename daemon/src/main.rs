use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info, warn};
use wispd::logging::{self, DEFAULT_LOG_LEVEL, LOG_LEVEL_ENV, LogFilter};
use wispd::paths::{DATA_DIR_ENV, DataDir};
use wispd::server::{self, Config, Server, Shutdown, StartError};
use wispd::service::{self, DEFAULT_LABEL, SERVICE_LABEL_ENV};

/// `serve` exits with this when another `wispd serve` already runs for the data folder.
const EXIT_ALREADY_RUNNING: u8 = 3;

#[derive(Debug, Parser)]
#[command(name = "wispd", version, about = "The wisp host daemon.")]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the editor on this user's socket until SIGTERM or SIGINT.
    Serve(ServeArgs),
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
        Command::Service(args) => service_command(args.command),
    }
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
        let shutdown = Shutdown::new();
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

fn service_command(command: ServiceCommand) -> ExitCode {
    match command {
        ServiceCommand::Install(options) => service_install(&options),
        ServiceCommand::Uninstall(options) => service_uninstall(&options),
        ServiceCommand::Status(options) => service_status(&options),
    }
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
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, ServiceCommand};

    #[test]
    fn the_command_line_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn serve_takes_a_data_folder_and_a_log_level() {
        let cli = Cli::try_parse_from([
            "wispd",
            "serve",
            "--data-dir",
            "/tmp/d",
            "--log-level",
            "wispd=debug,warn",
        ])
        .unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve, got {:?}", cli.command);
        };
        assert_eq!(
            args.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/d"))
        );
        assert_eq!(args.log_level.to_string(), "wispd=debug,warn");
    }

    #[test]
    fn unknown_levels_and_arguments_are_rejected() {
        assert!(Cli::try_parse_from(["wispd", "serve", "--log-level", "loud"]).is_err());
        assert!(Cli::try_parse_from(["wispd", "--bogus"]).is_err());
        assert!(Cli::try_parse_from(["wispd", "serve", "extra"]).is_err());
    }

    #[test]
    fn service_install_takes_a_data_folder_and_a_label() {
        let cli = Cli::try_parse_from([
            "wispd",
            "service",
            "install",
            "--data-dir",
            "/tmp/d",
            "--label",
            "io.example.test",
        ])
        .unwrap();
        let Command::Service(service) = cli.command else {
            panic!("expected service, got {:?}", cli.command);
        };
        let ServiceCommand::Install(options) = service.command else {
            panic!("expected install, got {:?}", service.command);
        };
        assert_eq!(
            options.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/d"))
        );
        assert_eq!(options.label, "io.example.test");
    }

    #[test]
    fn service_status_defaults_to_the_wisp_label_with_no_data_dir_override() {
        let cli = Cli::try_parse_from(["wispd", "service", "status"]).unwrap();
        let Command::Service(service) = cli.command else {
            panic!("expected service, got {:?}", cli.command);
        };
        let ServiceCommand::Status(options) = service.command else {
            panic!("expected status, got {:?}", service.command);
        };
        assert_eq!(options.label, super::DEFAULT_LABEL);
        assert_eq!(options.data_dir, None);
    }

    #[test]
    fn service_uninstall_parses_with_no_options() {
        let cli = Cli::try_parse_from(["wispd", "service", "uninstall"]).unwrap();
        let Command::Service(service) = cli.command else {
            panic!("expected service, got {:?}", cli.command);
        };
        assert!(matches!(service.command, ServiceCommand::Uninstall(_)));
    }

    #[test]
    fn service_without_a_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["wispd", "service"]).is_err());
    }
}
