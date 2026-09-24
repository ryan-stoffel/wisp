//! Starting, running, and stopping: the instance lock, the socket, signals, timers, and logs.

use std::fmt::Write as _;
use std::fs::{self, Permissions};
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rustix::process::Signal;
use sha2::{Digest, Sha256};
use tokio::time::{Instant, sleep};
use wisp_protocol::methods::{HostHealth, ProjectCreate};
use wisp_protocol::{HostHealthParams, ProjectCreateResult};

use crate::support::{
    Client, InProcess, PATIENCE, Wispd, WriteLock, create_params, run_to_exit, socket_path,
    temp_dir,
};

const SETTLE: Duration = Duration::from_millis(300);

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_second_instance_is_refused_and_the_first_keeps_serving() {
    let dir = temp_dir();
    let first = Wispd::start(dir.path()).await;

    let (status, stderr) = run_to_exit(dir.path(), &[]).await;
    assert_eq!(status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");
    assert!(stderr.contains(&format!("pid {}", first.pid())), "{stderr}");

    let mut client = Client::ready(&first.socket).await;
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stale_socket_is_replaced() {
    let dir = temp_dir();
    let socket = socket_path(dir.path());
    // A socket file nothing serves. It is a datagram socket because a stream listener here could
    // be inherited by another test's child before close-on-exec is set, and then accept the
    // connects meant for wispd (#86). wispd removes any kind of socket.
    drop(UnixDatagram::bind(&socket).unwrap());
    assert!(
        fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket()
    );

    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
}

#[tokio::test]
async fn a_file_that_is_not_a_socket_is_left_alone_and_stops_startup() {
    let dir = temp_dir();
    let socket = socket_path(dir.path());
    fs::write(&socket, "keep me").unwrap();

    let (status, stderr) = run_to_exit(dir.path(), &[]).await;
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("is not a socket"), "{stderr}");
    assert_eq!(fs::read_to_string(&socket).unwrap(), "keep me");
}

#[tokio::test]
async fn a_symlinked_data_folder_stops_startup() {
    let dir = temp_dir();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();

    let (status, stderr) = run_to_exit(&link, &[]).await;
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("symlink"), "{stderr}");
    assert!(
        fs::read_dir(&target).unwrap().next().is_none(),
        "nothing was written"
    );
}

#[tokio::test]
async fn the_data_folder_socket_lock_and_log_are_private() {
    let dir = temp_dir();
    let data = dir.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::set_permissions(&data, Permissions::from_mode(0o755)).unwrap();

    let wispd = Wispd::start(&data).await;
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&wispd.socket), 0o600);
    let lock = data.join("wispd.lock");
    assert_eq!(mode(&lock), 0o600);
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        format!("{}\n", wispd.pid())
    );
    assert_eq!(mode(&data.join("logs/wispd.log")), 0o600);
    assert_eq!(mode(&data.join("logs")), 0o700);
}

fn darwin_user_temp_dir() -> PathBuf {
    let output = Command::new("/usr/bin/getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim_end())
}

#[tokio::test]
async fn a_long_data_folder_puts_the_socket_in_the_user_temp_dir() {
    let dir = temp_dir();
    let data = dir.path().join("d".repeat(100));
    let hash = Sha256::digest(data.as_os_str().as_encoded_bytes())[..4]
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let expected = darwin_user_temp_dir().join(format!("wispd-{hash}.sock"));
    assert!(data.join("wispd.sock").as_os_str().len() > 103);

    let wispd = Wispd::start(&data).await;
    assert_eq!(wispd.socket, expected);
    assert_eq!(mode(&expected), 0o600);
    assert!(!data.join("wispd.sock").exists());
    let mut client = Client::ready(&expected).await;
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    drop(client);

    wispd.signal(Signal::TERM);
    assert!(wispd.exit().await.0.success());
    assert!(
        !expected.exists(),
        "the fallback socket is removed at shutdown"
    );
}

#[tokio::test]
async fn sigterm_finishes_the_requests_in_flight_then_cleans_up() {
    let dir = temp_dir();
    let mut wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let lock = WriteLock::take(dir.path());
    let params = create_params("wisp");
    let create = client.send::<ProjectCreate>(params.clone()).await;
    sleep(SETTLE).await;

    wispd.signal(Signal::TERM);
    let socket = wispd.socket.clone();
    eventually("the socket is removed", || !socket.exists()).await;
    assert!(wispd.is_running(), "it waits for the request in flight");
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());

    lock.release();
    let answered = client.response().await;
    assert_eq!(answered.id, Some(create));
    let created: ProjectCreateResult = answered.into_result().unwrap();
    assert_eq!(created.project.id, params.id);
    assert!(client.closes_within(PATIENCE).await);

    let (status, stderr) = wispd.exit().await;
    assert!(status.success(), "{status} {stderr}");
    assert!(!dir.path().join("wispd.lock").exists());
    let log = fs::read_to_string(dir.path().join("logs/wispd.log")).unwrap();
    assert!(log.contains("SIGTERM"), "{log}");
    assert!(log.contains("stopped"), "{log}");
}

#[tokio::test]
async fn sigint_stops_the_server_and_closes_idle_connections() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    wispd.signal(Signal::INT);
    assert!(client.closes_within(PATIENCE).await);
    let socket = wispd.socket.clone();
    let (status, stderr) = wispd.exit().await;
    assert!(status.success(), "{status} {stderr}");
    assert!(!socket.exists());
    assert!(!dir.path().join("wispd.lock").exists());
}

#[tokio::test]
async fn a_silent_connection_is_dropped_and_a_talking_one_is_not() {
    let dir = temp_dir();
    let mut config = InProcess::config(dir.path());
    config.idle_timeout = Duration::from_millis(400);
    let server = InProcess::start(config);
    let mut silent = Client::ready(&server.socket).await;
    let mut talking = Client::ready(&server.socket).await;

    for _ in 0..8 {
        sleep(Duration::from_millis(100)).await;
        talking
            .call::<HostHealth>(HostHealthParams {})
            .await
            .unwrap();
    }
    assert!(silent.closes_within(Duration::from_millis(100)).await);
    talking
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    drop(talking);
    server.stop().await;
}

#[tokio::test]
async fn a_deleted_socket_is_bound_again() {
    let dir = temp_dir();
    let mut config = InProcess::config(dir.path());
    config.socket_check_interval = Duration::from_millis(100);
    let server = InProcess::start(config);
    let mut before = Client::ready(&server.socket).await;

    fs::remove_file(&server.socket).unwrap();
    let socket = server.socket.clone();
    eventually("the socket is back", || socket.exists()).await;
    assert_eq!(mode(&server.socket), 0o600);
    let mut after = Client::ready(&server.socket).await;
    after.call::<HostHealth>(HostHealthParams {}).await.unwrap();
    before
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    drop((before, after));
    server.stop().await;
    assert!(!socket.exists());
}

async fn log_after_a_request(args: &[&str], env: &[(&str, &str)]) -> String {
    let dir = temp_dir();
    let wispd = Wispd::start_with(dir.path(), args, env).await;
    let mut client = Client::ready(&wispd.socket).await;
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    drop(client);
    wispd.signal(Signal::TERM);
    assert!(wispd.exit().await.0.success());
    fs::read_to_string(dir.path().join("logs/wispd.log")).unwrap()
}

#[tokio::test]
async fn logs_go_to_the_data_folder_at_the_level_from_the_flag_or_env() {
    let info = log_after_a_request(&[], &[]).await;
    assert!(
        info.contains(" INFO ") && info.contains("listening"),
        "{info}"
    );
    assert!(!info.contains(" DEBUG "), "{info}");

    let debug = log_after_a_request(&["--log-level", "debug"], &[]).await;
    assert!(debug.contains(" DEBUG "), "{debug}");

    let warn = log_after_a_request(&[], &[("WISPD_LOG", "warn")]).await;
    assert!(!warn.contains("listening"), "{warn}");

    let flag_wins = log_after_a_request(&["--log-level", "info"], &[("WISPD_LOG", "warn")]).await;
    assert!(flag_wins.contains("listening"), "{flag_wins}");
}

#[tokio::test]
async fn client_text_cant_forge_or_bloat_log_lines() {
    let dir = temp_dir();
    let wispd = Wispd::start_with(dir.path(), &["--log-level", "debug"], &[]).await;
    let forged = "2026-09-24T00:00:00.000000Z ERROR forged";
    let huge = "x".repeat(1_000_000);
    for (name, method) in [
        (format!("wisp\n{forged}"), format!("a\n{forged}")),
        (huge.clone(), huge),
    ] {
        let mut client = Client::connect(&wispd.socket).await;
        client
            .send_message(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocol": {"min": 1, "max": 1},
                    "client": {"name": name, "version": "0"},
                    "capabilities": {}
                }
            }))
            .await;
        assert!(client.response().await.result.is_ok());
        client
            .send_message(&serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": method}))
            .await;
        client.response().await;
        client
            .send_message(&serde_json::json!({"jsonrpc": "2.0", "method": method}))
            .await;
        client
            .call::<HostHealth>(HostHealthParams {})
            .await
            .unwrap();
    }
    wispd.signal(Signal::TERM);
    assert!(wispd.exit().await.0.success());

    let log = fs::read_to_string(dir.path().join("logs/wispd.log")).unwrap();
    assert!(
        log.contains("ERROR forged"),
        "the text is logged, escaped: {log}"
    );
    for line in log.lines() {
        assert!(!line.starts_with(forged), "a forged line: {line}");
        assert!(line.len() < 1_000, "a {}-byte line", line.len());
    }
}
