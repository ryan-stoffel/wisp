use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use parallax_protocol::{CoordinatorThreadId, ProjectId, RunId};
use plxd::attach::{
    self, DEFAULT_CONNECT_TIMEOUT, EXIT_UNAVAILABLE, MAX_CONNECT_TIMEOUT, Options, report,
};
use plxd::launch_agent::LaunchAgent;
use plxd::logging::{self, DEFAULT_LOG_LEVEL, LOG_LEVEL_ENV, LogFilter};
use plxd::paths::{DATA_DIR_ENV, DataDir};
use plxd::server::{self, Config, EXIT_ALREADY_RUNNING, Server, Shutdown, StartError};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use plxd::service::{self, DEFAULT_LABEL, SERVICE_LABEL_ENV};
use tracing::{error, info, warn};

#[derive(Debug, Parser)]
#[command(name = "plxd", version = plxd::version(), about = "The Parallax host daemon.")]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the editor on this user's socket or pipe until SIGTERM or SIGINT (Ctrl-C or
    /// Ctrl-Break on Windows).
    Serve(ServeArgs),
    /// Connect stdin and stdout to plxd's socket or pipe, starting plxd if it isn't running.
    Attach(AttachArgs),
    /// Manage the per-user service that keeps plxd running: a `LaunchAgent` on macOS, a systemd
    /// user unit on Linux.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Service(ServiceArgs),
    /// Serve a coordinator's or a thread's Parallax tools over MCP on stdin and stdout. plxd
    /// starts it.
    #[command(hide = true)]
    Mcp(McpArgs),
}

#[derive(Debug, Args)]
struct McpArgs {
    /// The data folder [default: ~/Library/Application Support/parallax on macOS, ~/.local/share/parallax
    /// on Linux, %LOCALAPPDATA%\parallax on Windows]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// The only project the coordinator's tools reach
    #[arg(
        long,
        value_name = "ID",
        required_unless_present = "thread",
        requires = "coordinator_thread"
    )]
    project: Option<ProjectId>,

    /// The coordinator thread that spawned runs are tagged with
    #[arg(long, value_name = "ID", requires = "project")]
    coordinator_thread: Option<CoordinatorThreadId>,

    /// The thread whose host-wide tools to serve: the caller of every tool (0041)
    #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["project", "coordinator_thread"])]
    thread: Option<RunId>,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// The data folder [default: ~/Library/Application Support/parallax on macOS, ~/.local/share/parallax
    /// on Linux, %LOCALAPPDATA%\parallax on Windows]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// off, error, warn, info, debug, or trace, or a list such as plxd=debug,warn
    #[arg(long, value_name = "LEVEL", env = LOG_LEVEL_ENV, default_value = DEFAULT_LOG_LEVEL)]
    log_level: LogFilter,
}

#[derive(Debug, Args)]
struct AttachArgs {
    /// The data folder [default: ~/Library/Application Support/parallax on macOS, ~/.local/share/parallax
    /// on Linux, %LOCALAPPDATA%\parallax on Windows]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// How long to wait for plxd to accept a connection, including starting it [default: 10]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Install or update the service, then start or restart it.
    Install(ServiceOptions),
    /// Stop the service if it is running, and remove it.
    Uninstall(ServiceOptions),
    /// Report whether the service is installed, loaded, and running.
    Status(ServiceOptions),
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Args)]
struct ServiceOptions {
    /// The data folder [default: ~/Library/Application Support/parallax on macOS, ~/.local/share/parallax
    /// on Linux, %LOCALAPPDATA%\parallax on Windows]
    #[arg(long, value_name = "DIR", env = DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,

    /// Override the service's label. For tests: a real install never needs this.
    #[arg(long, value_name = "LABEL", env = SERVICE_LABEL_ENV, default_value = DEFAULT_LABEL, hide = true)]
    label: String,
}

fn main() -> ExitCode {
    // Before anything else, so the version is the one this process started as (see `version`).
    plxd::version();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => serve(&args),
        Command::Attach(args) => attach(&args),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Command::Service(args) => service_command(args.command),
        Command::Mcp(args) => mcp(&args),
    }
}

/// Runs `mcp` and exits with `std::process::exit`, for the same reason as [`attach`].
fn mcp(args: &McpArgs) -> ! {
    let report = |message: &dyn std::fmt::Display| {
        let _ = writeln!(io::stderr(), "plxd mcp: {message}");
    };
    let socket = match DataDir::resolve(args.data_dir.as_deref()).and_then(|dir| dir.socket_path())
    {
        Ok(socket) => socket.path,
        Err(error) => {
            report(&format!("could not find plxd's socket: {error}"));
            std::process::exit(EXIT_UNAVAILABLE.into());
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report(&format!("could not start the runtime: {error}"));
            std::process::exit(1);
        }
    };
    let (stdin, stdout) = (tokio::io::stdin(), tokio::io::stdout());
    let served = match (args.thread, args.project, args.coordinator_thread) {
        (Some(run), _, _) => {
            let binding = plxd::mcp::thread::Binding { socket, run };
            runtime.block_on(plxd::mcp::thread::run(&binding, stdin, stdout))
        }
        (None, Some(project), Some(thread)) => {
            let binding = plxd::mcp::Binding {
                socket,
                project,
                thread,
            };
            runtime.block_on(plxd::mcp::run(&binding, stdin, stdout))
        }
        // clap requires `--thread`, or `--project` with `--coordinator-thread`.
        _ => Err("give --thread, or --project and --coordinator-thread".to_owned()),
    };
    if let Err(error) = served {
        report(&error);
        std::process::exit(1);
    }
    std::process::exit(0)
}

/// Runs `attach` and exits, with `std::process::exit`: the thread that reads stdin blocks until
/// input arrives, and would keep the runtime from shutting down (0007).
fn attach(args: &AttachArgs) -> ! {
    #[cfg(windows)]
    plxd::windows::stop_inheriting_handles();
    let data_dir = match DataDir::resolve(args.data_dir.as_deref()) {
        Ok(data_dir) => data_dir,
        Err(error) => unavailable(&format!("could not find the data folder: {error}")),
    };
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => unavailable(&format!("could not find plxd's own executable: {error}")),
    };
    let options = Options {
        program,
        connect_timeout: args.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT),
        launch_agent: LaunchAgent::installed_for(&data_dir),
    };
    let runtime = || match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => failed(&format!("could not start the runtime: {error}")),
    };
    // On Unix, starting plxd happens here, before the runtime exists (0023). Windows' pipe
    // client needs the runtime to connect.
    #[cfg(windows)]
    let runtime = runtime();
    #[cfg(windows)]
    let entered = runtime.enter();
    let stream = match attach::connect(&data_dir, &options) {
        Ok(stream) => stream,
        Err(error) => unavailable(&error),
    };
    #[cfg(windows)]
    drop(entered);
    #[cfg(unix)]
    let runtime = runtime();
    let code = runtime.block_on(async {
        let bridged = async {
            #[cfg(unix)]
            let stream = match stream
                .set_nonblocking(true)
                .and_then(|()| tokio::net::UnixStream::from_std(stream))
            {
                Ok(stream) => stream,
                Err(error) => failed(&format!("could not use the connection: {error}")),
            };
            match attach::bridge(tokio::io::stdin(), tokio::io::stdout(), stream).await {
                Ok(()) => 0,
                Err(error) => {
                    report(format_args!("the connection failed: {error}"));
                    1
                }
            }
        };
        // Handling SIGHUP, rather than leaving its default, also overrides an ignored SIGHUP
        // inherited from a parent such as nohup, so a dropped SSH session always ends attach.
        // On Windows, the session's end closes stdin and stdout, or kills attach with its job.
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut hangup = match signal(SignalKind::hangup()) {
                Ok(hangup) => hangup,
                Err(error) => failed(&format!("could not catch SIGHUP: {error}")),
            };
            tokio::select! {
                code = bridged => code,
                _ = hangup.recv() => 0,
            }
        }
        #[cfg(windows)]
        bridged.await
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
    // So agent CLIs don't inherit the log, or whatever else started `serve` (0023).
    #[cfg(windows)]
    plxd::windows::stop_inheriting_handles();
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
        // Every path plxd uses is absolute by now. Leaving the directory it was started in
        // keeps a folder, or the volume it is on, from staying busy for as long as plxd runs.
        if let Err(error) = std::env::set_current_dir("/") {
            warn!(%error, "could not change to the root folder");
        }
        let server = match Server::start(Config::new(data_dir)) {
            Ok(server) => server,
            Err(error @ StartError::AlreadyRunning { .. }) => {
                warn!(%error, "not starting");
                eprintln!("plxd: {error}");
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn service_command(command: ServiceCommand) -> ExitCode {
    match command {
        ServiceCommand::Install(options) => service_install(&options),
        ServiceCommand::Uninstall(options) => service_uninstall(&options),
        ServiceCommand::Status(options) => service_status(&options),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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
            println!("updated and restarted {}", options.label);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn service_status(options: &ServiceOptions) -> ExitCode {
    let data_dir = match resolve_data_dir(options) {
        Ok(data_dir) => data_dir,
        Err(code) => return code,
    };
    match service::status(&options.label, &data_dir) {
        Ok(status) => {
            println!("label: {}", status.label);
            println!("file: {}", status.path.display());
            println!("installed: {}", status.installed);
            println!("loaded: {}", status.state.loaded());
            println!("running: {}", status.state.running());
            println!(
                "pid: {}",
                status
                    .state
                    .pid()
                    .map_or_else(|| "-".to_owned(), |pid| pid.to_string())
            );
            println!("answers initialize: {}", status.answers_initialize);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn resolve_data_dir(options: &ServiceOptions) -> Result<DataDir, ExitCode> {
    DataDir::resolve(options.data_dir.as_deref())
        .map_err(|error| fail(&format!("could not find the data folder: {error}")))
}

#[cfg(unix)]
fn catch_signals(shutdown: Shutdown) -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

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

/// Windows' equivalents of SIGTERM and SIGINT (0023). A detached `serve` has no console, so
/// only a `serve` run in one receives them.
#[cfg(windows)]
fn catch_signals(shutdown: Shutdown) -> io::Result<()> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

    let mut c = ctrl_c()?;
    let mut r#break = ctrl_break()?;
    let mut close = ctrl_close()?;
    let mut logoff = ctrl_logoff()?;
    let mut shutdown_event = ctrl_shutdown()?;
    tokio::spawn(async move {
        loop {
            let name = tokio::select! {
                _ = c.recv() => "Ctrl-C",
                _ = r#break.recv() => "Ctrl-Break",
                _ = close.recv() => "console close",
                _ = logoff.recv() => "logoff",
                _ = shutdown_event.recv() => "shutdown",
            };
            info!(signal = name, "received a signal to stop");
            shutdown.trigger();
        }
    });
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("plxd: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::{CommandFactory, Parser};

    use super::{AttachArgs, Cli, Command};

    #[test]
    fn the_command_line_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_is_wired_to_plxds_own_version() {
        // daemon/tests/cli.rs checks what `plxd --version` actually prints; this just checks
        // the command is wired to `plxd::version` (0006, #44), not a hardcoded string.
        assert_eq!(Cli::command().get_version(), Some(plxd::version()));
    }

    #[test]
    fn serve_takes_a_data_folder_and_a_log_level() {
        let cli = Cli::try_parse_from([
            "plxd",
            "serve",
            "--data-dir",
            "/tmp/d",
            "--log-level",
            "plxd=debug,warn",
        ])
        .unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve, got {:?}", cli.command);
        };
        assert_eq!(
            args.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/d"))
        );
        assert_eq!(args.log_level.to_string(), "plxd=debug,warn");
    }

    #[test]
    fn unknown_levels_and_arguments_are_rejected() {
        assert!(Cli::try_parse_from(["plxd", "serve", "--log-level", "loud"]).is_err());
        assert!(Cli::try_parse_from(["plxd", "--bogus"]).is_err());
        assert!(Cli::try_parse_from(["plxd", "serve", "extra"]).is_err());
    }

    fn attach_args(args: &[&str]) -> Result<AttachArgs, clap::Error> {
        let cli = Cli::try_parse_from(["plxd", "attach"].iter().chain(args))?;
        let Command::Attach(args) = cli.command else {
            panic!("expected attach, got {:?}", cli.command);
        };
        Ok(args)
    }

    #[test]
    fn attach_takes_a_data_folder_and_a_timeout_in_seconds() {
        let args = attach_args(&["--data-dir", "/tmp/d", "--connect-timeout", "2.5"]).unwrap();
        assert_eq!(
            args.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/d"))
        );
        assert_eq!(args.connect_timeout, Some(Duration::from_millis(2500)));
        assert_eq!(attach_args(&[]).unwrap().connect_timeout, None);
        let longest = attach_args(&["--connect-timeout", "86400"]).unwrap();
        assert_eq!(longest.connect_timeout, Some(Duration::from_hours(24)));
    }

    #[test]
    fn attach_refuses_a_timeout_that_is_not_positive_and_extra_arguments() {
        for bad in ["0", "-1", "soon", "NaN", "inf", "1e19", "86401"] {
            assert!(attach_args(&["--connect-timeout", bad]).is_err(), "{bad}");
        }
        assert!(attach_args(&["extra"]).is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_install_takes_a_data_folder_and_a_label() {
        let cli = Cli::try_parse_from([
            "plxd",
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
        let super::ServiceCommand::Install(options) = service.command else {
            panic!("expected install, got {:?}", service.command);
        };
        assert_eq!(
            options.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/d"))
        );
        assert_eq!(options.label, "io.example.test");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_status_defaults_to_the_parallax_label_with_no_data_dir_override() {
        let cli = Cli::try_parse_from(["plxd", "service", "status"]).unwrap();
        let Command::Service(service) = cli.command else {
            panic!("expected service, got {:?}", cli.command);
        };
        let super::ServiceCommand::Status(options) = service.command else {
            panic!("expected status, got {:?}", service.command);
        };
        assert_eq!(options.label, super::DEFAULT_LABEL);
        assert_eq!(options.data_dir, None);
    }

    #[test]
    fn mcp_needs_a_project_and_a_coordinator_thread_or_a_thread_as_uuidv7s() {
        let project = "01a0d349-6e00-7c9e-80e2-0426486a8cae";
        let thread = "01a0d390-2c3d-7e4f-9a0b-1c2d3e4f5a6b";
        let cli = Cli::try_parse_from([
            "plxd",
            "mcp",
            "--project",
            project,
            "--coordinator-thread",
            thread,
        ])
        .unwrap();
        let Command::Mcp(args) = cli.command else {
            panic!("expected mcp, got {:?}", cli.command);
        };
        assert_eq!(args.project.unwrap().to_string(), project);
        assert_eq!(args.coordinator_thread.unwrap().to_string(), thread);
        assert_eq!(args.thread, None);
        let cli = Cli::try_parse_from(["plxd", "mcp", "--thread", thread]).unwrap();
        let Command::Mcp(args) = cli.command else {
            panic!("expected mcp, got {:?}", cli.command);
        };
        assert_eq!(args.thread.unwrap().to_string(), thread);
        assert!(Cli::try_parse_from(["plxd", "mcp"]).is_err());
        assert!(Cli::try_parse_from(["plxd", "mcp", "--project", project]).is_err());
        assert!(
            Cli::try_parse_from(["plxd", "mcp", "--thread", thread, "--project", project]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "plxd",
                "mcp",
                "--project",
                "not-an-id",
                "--coordinator-thread",
                thread,
            ])
            .is_err()
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_uninstall_parses_with_no_options() {
        let cli = Cli::try_parse_from(["plxd", "service", "uninstall"]).unwrap();
        let Command::Service(service) = cli.command else {
            panic!("expected service, got {:?}", cli.command);
        };
        assert!(matches!(
            service.command,
            super::ServiceCommand::Uninstall(_)
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_without_a_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["plxd", "service"]).is_err());
    }
}
