//! `agent/start`, `agent/send`, `agent/cancel`, `agent/list`, and `agent/events` (#156), behind
//! the `agents` capability; the review methods (#157) behind `agentReview`; `agent/openPr`
//! (RYA-168) behind `openPr`; `agent/image` (RYA-191) behind `promptImages`; `agent/approve`
//! (RYA-222) behind `approvals`; and `agent/gitStatus`, `agent/commit`, and `agent/push`
//! (RYA-298) behind `git`. The runner itself is [`crate::agents`].

use std::sync::Arc;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AgentAcceptParams, AgentAcceptResult, AgentApprovalAnswer, AgentApproveParams,
    AgentApproveResult, AgentAutoResumeParams, AgentCancelParams, AgentCommitParams,
    AgentDiffParams, AgentDiffResult, AgentEventsParams, AgentEventsResult, AgentFileParams,
    AgentFileResult, AgentFilesParams, AgentFilesResult, AgentGitStatusParams, AgentImageParams,
    AgentListParams, AgentListResult, AgentOpenPrParams, AgentOpenPrResult, AgentPolicy,
    AgentPushParams, AgentRequestChangesParams, AgentResumeNowParams, AgentRunResult,
    AgentSendParams, AgentStartParams, ErrorKind, GitStatus, LoggedEvent, PromptImage, RunId,
};

use super::Context;
use crate::agents::GitAction;
use crate::{agents, images};

/// The longest prompt or message plxd takes, in bytes. It goes on the CLI's stdin, never in
/// argv, and into the event log.
const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// The longest message a denial tells the agent, in bytes (RYA-222).
const MAX_DENIAL_BYTES: usize = 64 * 1024;

/// The longest pull request title GitHub takes, in characters.
const MAX_PR_TITLE_CHARS: usize = 256;

/// The longest pull request body plxd passes on, in bytes: GitHub's limit is 65,536 characters.
const MAX_PR_BODY_BYTES: usize = 64 * 1024;

const DEFAULT_EVENTS_LIMIT: u32 = 500;
const MAX_EVENTS_LIMIT: u32 = 1000;

/// About how much event JSON one `agent/events` page carries: half of 0007's 8 MiB frame, which
/// leaves room for the envelope. A page holds at least one event whatever its size.
pub(crate) const MAX_EVENTS_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Checks a prompt or message: its text, which may be empty only when it has images (RYA-193),
/// and its images (`images::check`).
pub(super) fn check_message(
    name: &str,
    text: &str,
    images: &[PromptImage],
) -> Result<(), ErrorObject> {
    if text.trim().is_empty() && images.is_empty() {
        return Err(ErrorObject::invalid_params(format!(
            "{name} must not be empty"
        )));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "{name} must be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    images::check(images)
}

// The runner's work runs detached from the request (`Agents::detached`), so a dropped
// connection or a `$/cancelRequest` never leaves a run half created.

pub(crate) async fn start(
    context: &Context,
    mut params: AgentStartParams,
) -> Result<AgentRunResult, ErrorObject> {
    if params.policy != AgentPolicy::WorkspaceWrite {
        return Err(ErrorObject::invalid_params(
            "policy must be workspaceWrite, the only policy agent/start takes",
        ));
    }
    check_message("prompt", &params.prompt, &params.images)?;
    params.threads = agents::attached::check(&context.daemon, params.threads).await?;
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
    mut params: AgentSendParams,
) -> Result<AgentRunResult, ErrorObject> {
    check_message("text", &params.text, &params.images)?;
    params.threads = agents::attached::check(&context.daemon, params.threads).await?;
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
        .detached(agents::cancel(daemon, params.run_id, params.from))
        .await?;
    Ok(AgentRunResult { run })
}

/// `agent/resumeNow` (PLX-371): through the run's actor, detached as `agent/cancel` is.
pub(crate) async fn resume_now(
    context: &Context,
    params: AgentResumeNowParams,
) -> Result<AgentRunResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    let run = context
        .daemon
        .agents
        .detached(agents::resume_now(daemon, params.run_id))
        .await?;
    Ok(AgentRunResult { run })
}

/// `agent/autoResume` (PLX-371): through the run's actor.
pub(crate) async fn auto_resume(
    context: &Context,
    params: AgentAutoResumeParams,
) -> Result<AgentRunResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    let AgentAutoResumeParams {
        run_id,
        auto_resume,
    } = params;
    let run = context
        .daemon
        .agents
        .detached(agents::set_auto_resume(daemon, run_id, auto_resume))
        .await?;
    Ok(AgentRunResult { run })
}

/// `agent/approve` (RYA-222): an edited input and `always` go only with `allow`, a message only
/// with `deny`; the run's actor does the rest.
pub(crate) async fn approve(
    context: &Context,
    params: AgentApproveParams,
) -> Result<AgentApproveResult, ErrorObject> {
    match params.decision {
        AgentApprovalAnswer::Allow => {
            if params.message.is_some() {
                return Err(ErrorObject::invalid_params(
                    "message goes only with decision deny",
                ));
            }
            if let Some(input) = &params.input {
                if !input.is_object() {
                    return Err(ErrorObject::invalid_params("input must be a JSON object"));
                }
                if json_len(input) > MAX_TEXT_BYTES {
                    return Err(ErrorObject::invalid_params(format!(
                        "input must be at most {MAX_TEXT_BYTES} bytes of JSON"
                    )));
                }
            }
        }
        AgentApprovalAnswer::Deny => {
            if params.input.is_some() || params.always {
                return Err(ErrorObject::invalid_params(
                    "input and always go only with decision allow",
                ));
            }
            if params
                .message
                .as_ref()
                .is_some_and(|message| message.len() > MAX_DENIAL_BYTES)
            {
                return Err(ErrorObject::invalid_params(format!(
                    "message must be at most {MAX_DENIAL_BYTES} bytes"
                )));
            }
        }
        AgentApprovalAnswer::Unknown => {
            return Err(ErrorObject::invalid_params(
                "decision must be allow or deny",
            ));
        }
    }
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(agents::approve(daemon, params))
        .await
}

/// The size of `value` as JSON.
fn json_len(value: &serde_json::Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |json| json.len())
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

pub(crate) async fn diff(
    context: &Context,
    params: AgentDiffParams,
) -> Result<AgentDiffResult, ErrorObject> {
    agents::review::diff(&context.daemon, params.run_id).await
}

pub(crate) async fn file(
    context: &Context,
    params: AgentFileParams,
) -> Result<AgentFileResult, ErrorObject> {
    agents::review::file(&context.daemon, params).await
}

pub(crate) async fn files(
    context: &Context,
    params: AgentFilesParams,
) -> Result<AgentFilesResult, ErrorObject> {
    agents::review::files(&context.daemon, params).await
}

pub(crate) async fn accept(
    context: &Context,
    params: AgentAcceptParams,
) -> Result<AgentAcceptResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(agents::accept(daemon, params))
        .await
}

/// `agent/openPr`: the title's first line, cut to GitHub's limit, and the body, checked here;
/// the push and `gh` run through the run's actor.
pub(crate) async fn open_pr(
    context: &Context,
    params: AgentOpenPrParams,
) -> Result<AgentOpenPrResult, ErrorObject> {
    let AgentOpenPrParams {
        run_id,
        title,
        body,
    } = params;
    let Some(title) = title.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Err(ErrorObject::invalid_params("title must not be empty"));
    };
    let title = title.chars().take(MAX_PR_TITLE_CHARS).collect();
    let body = body.unwrap_or_default();
    if body.len() > MAX_PR_BODY_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "body must be at most {MAX_PR_BODY_BYTES} bytes"
        )));
    }
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(agents::open_pr(daemon, run_id, title, body))
        .await
}

/// `agent/gitStatus` (RYA-298): through the run's actor, like `agent/commit` and `agent/push`.
pub(crate) async fn git_status(
    context: &Context,
    params: AgentGitStatusParams,
) -> Result<GitStatus, ErrorObject> {
    git(context, params.run_id, GitAction::Status).await
}

/// `agent/commit`: the message checked here, then the commit through the run's actor.
pub(crate) async fn commit(
    context: &Context,
    params: AgentCommitParams,
) -> Result<GitStatus, ErrorObject> {
    let AgentCommitParams { run_id, message } = params;
    if message.trim().is_empty() {
        return Err(ErrorObject::invalid_params("message must not be empty"));
    }
    if message.len() > MAX_PR_BODY_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "message must be at most {MAX_PR_BODY_BYTES} bytes"
        )));
    }
    git(context, run_id, GitAction::Commit(message)).await
}

/// `agent/push`.
pub(crate) async fn push(
    context: &Context,
    params: AgentPushParams,
) -> Result<GitStatus, ErrorObject> {
    git(context, params.run_id, GitAction::Push).await
}

async fn git(
    context: &Context,
    run_id: RunId,
    action: GitAction,
) -> Result<GitStatus, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(agents::git(daemon, run_id, action))
        .await
}

/// `agent/requestChanges`: the reviewer's follow-up, sent the way `agent/send` sends one.
pub(crate) async fn request_changes(
    context: &Context,
    params: AgentRequestChangesParams,
) -> Result<AgentRunResult, ErrorObject> {
    let AgentRequestChangesParams {
        run_id,
        turn_id,
        text,
    } = params;
    send(
        context,
        AgentSendParams {
            run_id,
            turn_id,
            text,
            model: None,
            effort: None,
            permission: None,
            context_window: None,
            fast: None,
            account: None,
            images: Vec::new(),
            threads: Vec::new(),
            from: None,
        },
    )
    .await
}

pub(crate) async fn image(
    context: &Context,
    params: AgentImageParams,
) -> Result<PromptImage, ErrorObject> {
    agents::image(&context.daemon, params).await
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
        return Err(ErrorObject::parallax(
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

    use parallax_protocol::framing::MAX_FRAME_BYTES;
    use parallax_protocol::jsonrpc::Response;
    use parallax_protocol::{AgentEventsParams, AgentOutputItem, ParallaxEvent, ProjectId, RunId};
    use parallax_store::{RunFields, RunState};
    use tokio_util::sync::CancellationToken;

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
                    coordinator_thread: None,
                    parent: None,
                    model: None,
                    effort: None,
                    permission: None,
                    context_window: None,
                    fast: None,
                    approvals: false,
                    checkout: false,
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
            daemon
                .log
                .append(
                    jiff::Timestamp::now(),
                    Some(project),
                    ParallaxEvent::AgentOutput {
                        run_id,
                        items: vec![AgentOutputItem::Text {
                            message_id: None,
                            text: text.clone(),
                        }],
                    },
                )
                .await;
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
