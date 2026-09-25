use std::process::{Command, Output};

fn wispd(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wispd"))
        .args(args)
        .env_remove("WISPD_LOG")
        .env_remove("WISPD_DATA_DIR")
        .output()
        .expect("wispd should run")
}

#[test]
fn version_prints_name_and_version() {
    let output = wispd(&["--version"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("wispd ", env!("CARGO_PKG_VERSION"), "\n")
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn unknown_argument_prints_usage_and_exits_2() {
    let output = wispd(&["--bogus"]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Usage: wispd"),
        "{output:?}"
    );
}

#[test]
fn no_arguments_print_help_and_exit_2() {
    let output = wispd(&[]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: wispd"), "{output:?}");
    assert!(stderr.contains("serve"), "{output:?}");
}

#[test]
fn an_unknown_log_level_is_a_usage_error() {
    let output = wispd(&[
        "serve",
        "--log-level",
        "loud",
        "--data-dir",
        "/nonexistent/wispd",
    ]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown log level"),
        "{output:?}"
    );
}

// This only ever reads: `launchctl print` on a label nobody bootstrapped, a socket connect
// that finds nobody listening, and a file existence check. It never calls `install`, so running
// `cargo test` never bootstraps a real LaunchAgent.
#[test]
fn service_status_reports_a_fresh_label_as_absent() {
    let temp = tempfile::Builder::new()
        .prefix("wispd-cli-")
        .tempdir_in("/tmp")
        .expect("create a temp dir under /tmp");
    let data_dir = temp.path().to_str().expect("a UTF-8 temp path");
    let output = wispd(&[
        "service",
        "status",
        "--data-dir",
        data_dir,
        "--label",
        "io.github.ryan-stoffel.wisp.wispd.cli-test-status",
    ]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("installed: false"), "{stdout}");
    assert!(stdout.contains("loaded: false"), "{stdout}");
    assert!(stdout.contains("running: false"), "{stdout}");
    assert!(stdout.contains("answers initialize: false"), "{stdout}");
}

// The refusal comes before anything touches launchd or `~/Library/LaunchAgents`, so this never
// installs a real LaunchAgent.
#[test]
fn service_install_refuses_the_default_label_for_another_data_folder() {
    let temp = tempfile::Builder::new()
        .prefix("wispd-cli-")
        .tempdir_in("/tmp")
        .expect("create a temp dir under /tmp");
    let data_dir = temp.path().join("data");
    let output = Command::new(env!("CARGO_BIN_EXE_wispd"))
        .args(["service", "install", "--data-dir"])
        .arg(&data_dir)
        .env_remove("WISPD_LOG")
        .env_remove("WISPD_DATA_DIR")
        .env_remove("WISPD_SERVICE_LABEL")
        .output()
        .expect("wispd should run");

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("serves only the default data folder"),
        "{stderr}"
    );
    assert!(
        !data_dir.exists(),
        "nothing was prepared for the refused folder"
    );
}
