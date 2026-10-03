//! `thread/fork` (PLX-375, decision 0050): a fork's workspace and transcript, and how its first
//! message continues the parent's conversation, natively or by handoff.

use parallax_protocol::methods::ThreadFork;
use parallax_protocol::{AgentOutputItem, ThreadForkParams, ThreadStartResult};

use super::*;

/// What a backend's CLI started with.
#[derive(Clone, Debug)]
struct Start {
    prompt: String,
    /// The session it resumed or forked.
    resume: Option<String>,
    fork: bool,
}

/// The starts a backend recorded.
type Starts = Arc<Mutex<Vec<Start>>>;

/// The fake CLI as a backend named `name`, recording each start.
struct Seen {
    name: &'static str,
    fake: FakeBackend,
    starts: Starts,
}

impl Backend for Seen {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        self.starts.lock().unwrap().push(Start {
            prompt: request.prompt.clone(),
            resume: request.resume.as_ref().map(|r| r.session_id.clone()),
            fork: request.resume.as_ref().is_some_and(|r| r.fork),
        });
        self.fake.start(request)
    }
}

/// A host whose `fake` backend, and `other` backend for another provider, record their starts.
fn host() -> (Host, Starts, Starts) {
    let (fake, other) = (Arc::default(), Arc::default());
    let mut backends = BackendRegistry::new();
    for (provider, name, starts) in [
        (Provider::Anthropic, "fake", &fake),
        (Provider::Openai, "other", &other),
    ] {
        let seen = Seen {
            name,
            fake: fake_backend(editing()),
            starts: Arc::clone(starts),
        };
        backends.register(provider, Arc::new(seen));
    }
    (Host::start(backends), fake, other)
}

fn finished(run_id: RunId) -> impl FnMut(&EventsEventParams) -> bool {
    move |event| {
        matches!(&event.event, ParallaxEvent::AgentUpdated { run_id: id, state }
            if *id == run_id && state.status == AgentStatus::Completed)
    }
}

/// Starts a thread with "Write the notes", then sends it "Now the tests", and returns it and the
/// follow-up's turn id once both turns are done. Subscribes to its scope.
async fn two_turns(client: &mut Conn, params: ThreadStartParams) -> (ThreadStartResult, TurnId) {
    let started = client.call::<ThreadStart>(params).await.unwrap();
    let run_id = started.thread.id;
    client.subscribe(0, Some(scope(started.thread.repo))).await;
    client.until(finished(run_id)).await;
    let follow_up = message(run_id, "Now the tests");
    client.call::<AgentSend>(follow_up.clone()).await.unwrap();
    client.until(finished(run_id)).await;
    (started, follow_up.turn_id)
}

fn fork_params(run_id: RunId) -> ThreadForkParams {
    ThreadForkParams {
        run_id,
        new_run_id: RunId::generate(),
        turn_id: None,
        account: None,
        model: None,
    }
}

/// The `agent.output` items in run `run_id`'s log, oldest first, and its `agent.started` prompt.
async fn transcript(client: &mut Conn, run_id: RunId) -> (String, Vec<AgentOutputItem>) {
    let events = client
        .call::<AgentEvents>(AgentEventsParams {
            run_id,
            after: 0,
            limit: None,
        })
        .await
        .unwrap()
        .events;
    let Some(ParallaxEvent::AgentStarted { run: Some(run), .. }) = events.first().map(|e| &e.event)
    else {
        panic!("the log starts with agent.started: {events:?}");
    };
    let items = events
        .iter()
        .filter_map(|logged| match &logged.event {
            ParallaxEvent::AgentOutput { run_id: id, items } => {
                assert_eq!(*id, run_id, "copied items carry the fork's id");
                Some(items.clone())
            }
            _ => None,
        })
        .flatten()
        .collect();
    (run.prompt.clone(), items)
}

fn follow_ups(items: &[AgentOutputItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            AgentOutputItem::TurnStarted {
                text: Some(text), ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A fork at the latest turn of a thread in a worktree: its own worktree from the parent's latest
/// commit, the parent's whole transcript, and a first message that forks the parent's session.
#[tokio::test]
async fn a_fork_at_the_latest_turn_forks_the_parents_session() {
    let (host, fake, _) = host();
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let (parent, follow_up) =
        two_turns(&mut client, start_params(Some(repo.id), "Write the notes")).await;

    let params = fork_params(parent.thread.id);
    let forked = client.call::<ThreadFork>(params.clone()).await.unwrap();
    let from = forked.thread.forked_from.expect("forkedFrom");
    assert_eq!((from.run, from.turn), (parent.thread.id, follow_up));
    assert_eq!(forked.thread.repo, repo.id);
    assert_eq!(forked.run.status, AgentStatus::Completed, "no CLI yet");
    assert_eq!(forked.run.prompt, "Write the notes");
    assert_ne!(forked.run.branch, parent.run.branch);
    let worktree = PathBuf::from(forked.run.worktree_path.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(worktree.join("NOTES.md")).unwrap(),
        "Written in a thread.\n",
        "cut from the parent's latest commit"
    );
    let (prompt, items) = transcript(&mut client, params.new_run_id).await;
    assert_eq!(prompt, "Write the notes");
    assert_eq!(follow_ups(&items), ["Now the tests"]);

    let retried = client.call::<ThreadFork>(params.clone()).await.unwrap();
    assert_eq!(retried.thread, forked.thread, "a retry returns the fork");
    let other_turn = ThreadForkParams {
        turn_id: Some(first_turn(parent.thread.id)),
        ..params.clone()
    };
    let conflict = client.call::<ThreadFork>(other_turn).await.unwrap_err();
    assert_eq!(kind(&conflict), ErrorKind::IdConflict);

    client
        .call::<AgentSend>(message(params.new_run_id, "Carry on"))
        .await
        .unwrap();
    client.until(finished(params.new_run_id)).await;
    let last = fake.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.resume.as_deref(), Some("thread-1"), "{last:?}");
    assert!(last.fork, "{last:?}");
    assert_eq!(last.prompt, "Carry on");
    host.server.stop().await;
}

/// A Current checkout thread forked onto another provider works in the same checkout, and its
/// first message hands the parent's conversation to the new CLI.
#[tokio::test]
async fn a_fork_onto_another_provider_takes_a_handoff_in_the_same_checkout() {
    let (host, _, other) = host();
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let params = ThreadStartParams {
        checkout: true,
        ..start_params(Some(repo.id), "Write the notes")
    };
    let (parent, _) = two_turns(&mut client, params).await;

    let params = ThreadForkParams {
        account: Some(AccountChoice::Subscription {
            backend: "other".to_owned(),
        }),
        model: Some("gpt-6".to_owned()),
        ..fork_params(parent.thread.id)
    };
    let forked = client.call::<ThreadFork>(params.clone()).await.unwrap();
    assert!(forked.run.checkout);
    assert_eq!(forked.run.worktree_path, None);
    assert_eq!(forked.run.backend, "other");
    assert_eq!(forked.run.model.as_deref(), Some("gpt-6"));

    client
        .call::<AgentSend>(message(params.new_run_id, "Carry on"))
        .await
        .unwrap();
    client.until(finished(params.new_run_id)).await;
    let starts = other.lock().unwrap().clone();
    let [start] = starts.as_slice() else {
        panic!("one CLI on the other backend: {starts:?}");
    };
    assert_eq!(start.resume, None, "a new session");
    assert!(
        start.prompt.contains(
            "User:\nWrite the notes\n\nAgent:\nDone.\n\nUser:\nNow the tests\n\nAgent:\nDone.\n</conversation>"
        ),
        "{}",
        start.prompt
    );
    assert!(
        start.prompt.ends_with("yours to answer:\nCarry on"),
        "{}",
        start.prompt
    );
    host.server.stop().await;
}

/// A fork at an earlier turn of a thread with no repo: its own scratch repository with the
/// parent's latest commit, the parent's transcript up to that turn, and a handoff of only that.
#[tokio::test]
async fn a_fork_at_an_earlier_turn_hands_over_the_conversation_up_to_it() {
    let (host, fake, _) = host();
    let mut client = host.client().await;
    let (parent, _) = two_turns(&mut client, start_params(None, "Write the notes")).await;

    let unknown = ThreadForkParams {
        turn_id: Some(TurnId::generate()),
        ..fork_params(parent.thread.id)
    };
    let refused = client.call::<ThreadFork>(unknown).await.unwrap_err();
    assert_eq!(refused.code, INVALID_PARAMS, "{refused:?}");

    let params = ThreadForkParams {
        turn_id: Some(first_turn(parent.thread.id)),
        ..fork_params(parent.thread.id)
    };
    let forked = client.call::<ThreadFork>(params.clone()).await.unwrap();
    assert_eq!(forked.thread.repo, parent.thread.repo, "the scratch entry");
    let worktree = PathBuf::from(forked.run.worktree_path.clone().unwrap());
    assert!(
        worktree.join("NOTES.md").exists(),
        "cut from the parent's latest commit"
    );
    let scratch = host
        .data()
        .join("scratch")
        .join(params.new_run_id.to_string());
    assert!(
        scratch.join(".git").is_dir(),
        "a scratch repository of its own"
    );
    let (_, items) = transcript(&mut client, params.new_run_id).await;
    assert!(follow_ups(&items).is_empty(), "{items:?}");
    assert!(
        items
            .iter()
            .any(|item| matches!(item, AgentOutputItem::Text { text, .. } if text == "Done.")),
        "{items:?}"
    );

    client
        .call::<AgentSend>(message(params.new_run_id, "Carry on"))
        .await
        .unwrap();
    client.until(finished(params.new_run_id)).await;
    let last = fake.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.resume, None, "a handoff, not the parent's session");
    assert!(
        last.prompt
            .contains("User:\nWrite the notes\n\nAgent:\nDone.\n</conversation>"),
        "{}",
        last.prompt
    );
    assert!(!last.prompt.contains("Now the tests"), "{}", last.prompt);
    host.server.stop().await;
}

/// The prompt's turn of run `run_id`, which `thread/fork` names by the run's id.
fn first_turn(run_id: RunId) -> TurnId {
    TurnId::try_from(uuid::Uuid::from(run_id)).unwrap()
}
