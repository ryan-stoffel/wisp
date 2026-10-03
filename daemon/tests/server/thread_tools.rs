//! `plxd mcp --thread`, a thread's host-wide Parallax tools (0041, PLX-373): the built binary,
//! speaking MCP on stdio, against an in-process plxd whose threads run on the fake backend in a
//! real git repository.

use parallax_protocol::methods::{AgentEvents, RepoAdd, ThreadList, ThreadStart};
use parallax_protocol::{
    AccountChoice, AgentEventsParams, AgentOutputItem, ParallaxEvent, RepoAddParams, RepoId, RunId,
    ThreadListParams, ThreadStartParams,
};
use plxd::backend::fake::Step;
use plxd::mcp::thread::TOOLS;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::agents::{Conn, Host, end_turn, fake, init, real_repo};
use crate::mcp::{Mcp, mcp_command};
use crate::support::temp_dir;

/// Echoes each message, takes a moment, and finishes its turn.
fn echo() -> Vec<Step> {
    vec![
        init("echo-1"),
        Step::EchoPrompt,
        Step::SleepMs(300),
        end_turn("Done."),
    ]
}

/// Echoes its prompt, then runs until it is stopped.
fn hang() -> Vec<Step> {
    vec![init("hang-1"), Step::EchoPrompt, Step::Hang]
}

/// A thread the user started in a new repo entry under `repos`, which the tools are bound to.
async fn caller(client: &mut Conn, repos: &TempDir) -> (RunId, RepoId) {
    let repo = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: real_repo(repos.path()).to_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .repo;
    let started = client
        .call::<ThreadStart>(ThreadStartParams {
            run_id: RunId::generate(),
            repo: Some(repo.id),
            parent: None,
            title: None,
            prompt: "Plan the work.".to_owned(),
            account: Some(AccountChoice::Subscription {
                backend: "fake".to_owned(),
            }),
            model: None,
            effort: None,
            permission: None,
            context_window: None,
            fast: None,
            branch_slug: None,
            images: Vec::new(),
            threads: Vec::new(),
            approvals: false,
            checkout: false,
            base: None,
            checkout_ref: None,
        })
        .await
        .unwrap();
    (started.run.id, repo.id)
}

async fn tools(host: &Host, run: RunId) -> Mcp {
    let run = run.to_string();
    Mcp::spawn(mcp_command(host.dir.path(), &["--thread", &run])).await
}

/// Every transcript item of `run` in plxd's log.
async fn transcript(client: &mut Conn, run: RunId) -> Vec<AgentOutputItem> {
    let events = client
        .call::<AgentEvents>(AgentEventsParams {
            run_id: run,
            after: 0,
            limit: Some(1000),
        })
        .await
        .unwrap();
    events
        .events
        .into_iter()
        .filter_map(|logged| match logged.event {
            ParallaxEvent::AgentOutput { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .collect()
}

fn id(value: &Value) -> RunId {
    value["runId"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_launches_waits_on_reads_searches_and_messages_a_child() {
    let host = Host::start(temp_dir(), fake(echo()));
    let mut client = host.client().await;
    let repos = temp_dir();
    let (me, repo) = caller(&mut client, &repos).await;
    let mut mcp = tools(&host, me).await;

    let listed = mcp.request("tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, TOOLS);

    let child = mcp
        .ok(
            "thread_launch",
            json!({"prompt": "Write the notes.", "title": "Notes", "backend": "fake"}),
        )
        .await;
    assert_eq!(child["parent"], me.to_string(), "{child}");
    assert_eq!(child["title"], "Notes");
    assert_eq!(child["you"], false);
    assert_eq!(
        child["repo"]["id"],
        repo.to_string(),
        "the caller's repo by default"
    );
    assert!(child["branch"].is_string(), "a new worktree: {child}");
    let child = id(&child);
    let stored = client
        .call::<ThreadList>(ThreadListParams {})
        .await
        .unwrap();
    let thread = stored.threads.iter().find(|t| t.id == child).unwrap();
    assert_eq!(
        thread.parent,
        Some(me),
        "plxd records the caller as the parent"
    );

    let waited = mcp.ok("thread_wait", json!({"runId": child})).await;
    assert_eq!(waited["idle"], true, "{waited}");
    assert_eq!(waited["thread"]["status"], "completed");
    assert_eq!(waited["lastOutput"], "Done.");
    let (read, is_error) = mcp.tool("thread_read", json!({"runId": child})).await;
    assert!(!is_error, "{read}");
    assert!(
        read.starts_with("First message:\nWrite the notes.\n"),
        "{read}"
    );
    assert!(read.contains("Agent:\nWrite the notes.\n"), "{read}");
    assert!(read.contains("[run completed]"), "{read}");

    mcp.ok(
        "thread_send",
        json!({"runId": child, "text": "Add a summary."}),
    )
    .await;
    let waited = mcp.ok("thread_wait", json!({"runId": child})).await;
    assert_eq!(waited["idle"], true, "{waited}");
    let sent = transcript(&mut client, child)
        .await
        .into_iter()
        .find_map(|item| match item {
            AgentOutputItem::TurnStarted {
                text: Some(text),
                from,
                ..
            } => Some((text, from)),
            _ => None,
        });
    assert_eq!(
        sent,
        Some(("Add a summary.".to_owned(), Some(me))),
        "the message is marked with the thread that sent it"
    );
    let (read, _) = mcp.tool("thread_read", json!({"runId": child})).await;
    assert!(
        read.contains(&format!("Thread {me}:\nAdd a summary.\n")),
        "{read}"
    );

    let list = mcp.ok("thread_list", json!({})).await;
    let threads = list["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0]["runId"], child.to_string(), "newest first");
    assert_eq!(threads[1]["you"], true);
    let found = mcp
        .ok("thread_search", json!({"query": "add a SUMMARY"}))
        .await;
    let found: Vec<&Value> = found["threads"].as_array().unwrap().iter().collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["runId"], child.to_string());
    host.server.stop().await;
}

/// The caller is bound by `--thread`: it can't name itself as a sender, and can't wait on,
/// message, or stop its own turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_cant_act_on_itself_or_name_a_sender() {
    let host = Host::start(temp_dir(), fake(echo()));
    let mut client = host.client().await;
    let repos = temp_dir();
    let (me, _) = caller(&mut client, &repos).await;
    let mut mcp = tools(&host, me).await;
    let child = RunId::generate();
    for (tool, arguments) in [
        ("thread_send", json!({"runId": me, "text": "Hi."})),
        ("thread_wait", json!({"runId": me})),
        ("thread_interrupt", json!({"runId": me})),
    ] {
        let refused = mcp.refused(tool, arguments).await;
        assert!(refused.contains("itself"), "{tool}: {refused}");
    }
    let refused = mcp
        .refused(
            "thread_send",
            json!({"runId": child, "text": "Hi.", "from": me}),
        )
        .await;
    assert!(refused.contains("unknown field"), "{refused}");
    let missing = mcp
        .refused("thread_read", json!({"runId": RunId::generate()}))
        .await;
    assert!(missing.contains("no thread has run id"), "{missing}");
    host.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_interrupts_renames_settles_archives_and_links_a_pr_to_another() {
    let host = Host::start(temp_dir(), fake(hang()));
    let mut client = host.client().await;
    let repos = temp_dir();
    let (me, _) = caller(&mut client, &repos).await;
    let mut mcp = tools(&host, me).await;

    let child = mcp
        .ok(
            "thread_launch",
            json!({"prompt": "Wait here.", "backend": "fake", "workspace": "none"}),
        )
        .await;
    assert_eq!(child["repo"], Value::Null, "{child}");
    let child = id(&child);
    let waited = mcp
        .ok("thread_wait", json!({"runId": child, "timeoutSeconds": 1}))
        .await;
    assert_eq!(waited["idle"], false, "{waited}");
    assert_eq!(waited["timedOut"], true);
    assert_eq!(waited["thread"]["status"], "running");

    mcp.ok("thread_interrupt", json!({"runId": child})).await;
    let waited = mcp.ok("thread_wait", json!({"runId": child})).await;
    assert_eq!(waited["thread"]["status"], "cancelled", "{waited}");
    assert!(
        transcript(&mut client, child)
            .await
            .contains(&AgentOutputItem::Interrupted { from: me }),
        "the interrupt is marked with the thread that sent it"
    );
    let (read, _) = mcp.tool("thread_read", json!({"runId": child})).await;
    assert!(
        read.contains(&format!("[stopped by thread {me}]")),
        "{read}"
    );
    assert!(read.contains("[run stopped]"), "{read}");

    let updated = mcp
        .ok(
            "thread_update",
            json!({"runId": child, "title": "Waiter", "settled": true, "archived": true}),
        )
        .await;
    assert_eq!(updated["title"], "Waiter");
    assert_eq!(updated["settled"], true);
    assert_eq!(updated["archived"], true);
    let renamed = mcp.ok("thread_update", json!({"title": "Planner"})).await;
    assert_eq!(renamed["id"], me.to_string(), "no runId means the caller");
    assert!(
        mcp.refused("thread_update", json!({}))
            .await
            .contains("give")
    );
    let list = mcp.ok("thread_list", json!({})).await;
    assert_eq!(list["threads"].as_array().unwrap().len(), 1, "{list}");
    let list = mcp
        .ok("thread_list", json!({"includeArchived": true}))
        .await;
    assert_eq!(list["threads"].as_array().unwrap().len(), 2, "{list}");

    let url = "https://github.com/owner/repo/pull/7";
    let linked = mcp.ok("pr_link", json!({"url": url, "runId": child})).await;
    assert_eq!(linked["pullRequests"], json!([url]));
    let mine = mcp.ok("pr_link", json!({"url": url})).await;
    assert_eq!(mine["runId"], me.to_string());
    let refused = mcp
        .refused("pr_link", json!({"url": "https://example.com/pull/7"}))
        .await;
    assert!(
        refused.contains("not a GitHub pull request URL"),
        "{refused}"
    );
    let unlinked = mcp
        .ok("pr_unlink", json!({"url": url, "runId": child}))
        .await;
    assert_eq!(unlinked["pullRequests"], json!([]));

    let refused = mcp
        .refused(
            "thread_launch",
            json!({"prompt": "Go.", "workspace": "checkout", "base": "main"}),
        )
        .await;
    assert!(
        refused.contains("base goes with workspace worktree"),
        "{refused}"
    );
    let refused = mcp
        .refused(
            "thread_launch",
            json!({"prompt": "Go.", "backend": "fake", "mode": "bypass"}),
        )
        .await;
    assert!(
        refused.contains("you run in edit mode") && refused.contains("can't run in bypass"),
        "a child gets no more permission than its caller: {refused}"
    );
    host.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_refuses_a_thread_plxd_doesnt_know() {
    let host = Host::start(temp_dir(), fake(echo()));
    let run = RunId::generate().to_string();
    let output = mcp_command(host.dir.path(), &["--thread", &run])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no thread has run id"), "{stderr}");
    host.server.stop().await;
}
