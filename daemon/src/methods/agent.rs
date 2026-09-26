//! `agent/start`, `agent/send`, `agent/cancel`, `agent/list`, and `agent/events` (#156), behind
//! the `agents` capability. The runner itself is [`crate::agents`].

use std::sync::Arc;

use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AgentCancelParams, AgentEventsParams, AgentEventsResult, AgentListParams, AgentListResult,
    AgentPolicy, AgentRunResult, AgentSendParams, AgentStartParams, ErrorKind, LoggedEvent,
};

use super::Context;
use crate::agents;

/// The longest prompt or message wispd takes, in bytes. It goes on the CLI's stdin, never in
/// argv, and into the event log.
const MAX_TEXT_BYTES: usize = 1024 * 1024;

const DEFAULT_EVENTS_LIMIT: u32 = 500;
const MAX_EVENTS_LIMIT: u32 = 1000;

/// About how much event JSON one `agent/events` page carries: half of 0007's 8 MiB frame, which
/// leaves room for the envelope. A page holds at least one event whatever its size.
pub(crate) const MAX_EVENTS_PAGE_BYTES: usize = 4 * 1024 * 1024;

fn check_text(name: &str, text: &str) -> Result<(), ErrorObject> {
    if text.trim().is_empty() {
        return Err(ErrorObject::invalid_params(format!(
            "{name} must not be empty"
        )));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "{name} must be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

// The runner's work runs detached from the request (`Agents::detached`), so a dropped
// connection or a `$/cancelRequest` never leaves a run half created.

pub(crate) async fn start(
    context: &Context,
    params: AgentStartParams,
) -> Result<AgentRunResult, ErrorObject> {
    if params.policy != AgentPolicy::WorkspaceWrite {
        return Err(ErrorObject::invalid_params(
            "policy must be workspaceWrite, the only policy agent/start takes",
        ));
    }
    check_text("prompt", &params.prompt)?;
    let daemon = Arc::clone(&context.daemon);
    let run = context
        .daemon
        .agents
        .detached(agents::start(daemon, params))
        .await?;
    Ok(AgentRunResult { run })
}

pub(crate) async fn send(
    context: &Context,
    params: AgentSendParams,
) -> Result<AgentRunResult, ErrorObject> {
    check_text("text", &params.text)?;
    let daemon = Arc::clone(&context.daemon);
    let run = context
        .daemon
        .agents
        .detached(agents::send(daemon, params))
        .await?;
    Ok(AgentRunResult { run })
}

pub(crate) async fn cancel(
    context: &Context,
    params: AgentCancelParams,
) -> Result<AgentRunResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    let run = context
        .daemon
        .agents
        .detached(agents::cancel(daemon, params.run_id))
        .await?;
    Ok(AgentRunResult { run })
}

pub(crate) async fn list(
    context: &Context,
    params: AgentListParams,
) -> Result<AgentListResult, ErrorObject> {
    let log = Arc::clone(&context.daemon.log);
    let project = params.project.map(uuid::Uuid::from);
    let (runs, seq) = context
        .daemon
        .store
        .run(&context.cancel, move |db| {
            let rows = db
                .list_runs(project)
                .map_err(|error| crate::agents::store_error(&error))?;
            let seq = log.head();
            let mut runs = Vec::with_capacity(rows.len());
            for row in rows {
                let worktree = db
                    .get_worktree(row.id)
                    .map_err(|error| crate::agents::store_error(&error))?;
                runs.push((row, worktree));
            }
            Ok((runs, seq))
        })
        .await?;
    let runs = runs
        .iter()
        .map(|(row, worktree)| agents::snapshot(row, worktree.as_ref()))
        .collect::<Result<_, _>>()?;
    Ok(AgentListResult { runs, seq })
}

pub(crate) async fn events(
    context: &Context,
    params: AgentEventsParams,
) -> Result<AgentEventsResult, ErrorObject> {
    let AgentEventsParams {
        run_id,
        after,
        limit,
    } = params;
    let limit = limit
        .unwrap_or(DEFAULT_EVENTS_LIMIT)
        .clamp(1, MAX_EVENTS_LIMIT) as usize;
    let exists = context
        .daemon
        .store
        .run(&context.cancel, move |db| {
            db.get_run(run_id.into())
                .map(|row| row.is_some())
                .map_err(|error| crate::agents::store_error(&error))
        })
        .await?;
    if !exists {
        return Err(ErrorObject::wisp(
            ErrorKind::RunNotFound,
            format!("no agent run has id {run_id}"),
        ));
    }
    let log = Arc::clone(&context.daemon.log);
    let (entries, more) = tokio::task::spawn_blocking(move || {
        log.run_events(run_id, after, limit, MAX_EVENTS_PAGE_BYTES)
    })
    .await
    .map_err(ErrorObject::internal_error)?
    .map_err(|error| crate::agents::store_error(&error))?;
    let events = entries
        .iter()
        .map(|entry| LoggedEvent {
            seq: entry.seq,
            time: entry.time,
            project: entry.project,
            event: entry.event.clone(),
        })
        .collect();
    Ok(AgentEventsResult { events, more })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;
    use wisp_protocol::framing::MAX_FRAME_BYTES;
    use wisp_protocol::jsonrpc::Response;
    use wisp_protocol::{AgentEventsParams, AgentOutputItem, ProjectId, RunId, WispEvent};
    use wisp_store::{RunFields, RunState};

    use super::{Context, MAX_EVENTS_PAGE_BYTES, events};
    use crate::server::Daemon;

    #[tokio::test]
    async fn pages_of_large_output_fit_in_a_frame_and_page_through_everything() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 10_000, Duration::from_secs(90));
        let context = Context {
            daemon: Arc::clone(&daemon),
            cancel: CancellationToken::new(),
        };
        let (run_id, project) = (RunId::generate(), ProjectId::generate());
        daemon
            .store
            .run(&CancellationToken::new(), move |db| {
                let fields = RunFields {
                    project_id: project.into(),
                    prompt: "p".to_owned(),
                    requested_account: None,
                    policy: "workspaceWrite".to_owned(),
                    backend: "fake".to_owned(),
                };
                let state = RunState {
                    status: "running".to_owned(),
                    account_id: "fake".to_owned(),
                    ..RunState::default()
                };
                db.create_run(run_id.into(), &fields, &state).unwrap();
                Ok(())
            })
            .await
            .unwrap();
        // 60 batches of about 250 KiB, as a tool-heavy run's `agent.output` events can be: 15
        // MiB in all, which a page counted by events alone would put in one oversized frame.
        let text = "x".repeat(250 * 1024);
        for _ in 0..60 {
            daemon.log.append(
                jiff::Timestamp::now(),
                Some(project),
                WispEvent::AgentOutput {
                    run_id,
                    items: vec![AgentOutputItem::Text {
                        message_id: None,
                        text: text.clone(),
                    }],
                },
            );
        }

        let mut after = 0;
        let mut seen = 0;
        let mut pages = 0;
        loop {
            let page = events(
                &context,
                AgentEventsParams {
                    run_id,
                    after,
                    limit: None,
                },
            )
            .await
            .unwrap();
            let response = Response::success(1.into(), serde_json::to_value(&page).unwrap());
            let frame = serde_json::to_vec(&response).unwrap();
            assert!(frame.len() < MAX_FRAME_BYTES, "{} bytes", frame.len());
            assert!(frame.len() < MAX_EVENTS_PAGE_BYTES + 512 * 1024);
            assert!(!page.events.is_empty());
            seen += page.events.len();
            pages += 1;
            after = page.events.last().unwrap().seq;
            if !page.more {
                break;
            }
        }
        assert_eq!(seen, 60);
        assert!(pages >= 4, "{pages} pages");
    }
}
