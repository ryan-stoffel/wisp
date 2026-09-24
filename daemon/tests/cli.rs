use std::process::{Command, Output};

fn wispd(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wispd"))
        .args(args)
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
        String::from_utf8_lossy(&output.stderr).starts_with("Usage: wispd"),
        "{output:?}"
    );
}
