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
