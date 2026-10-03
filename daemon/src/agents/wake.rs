//! Waking a project's coordinator when runs it started finish (RYA-42, decision 0025).
//!
//! When a worker with a `coordinatorThread` ends a CLI process, [`notify`] hands a summary of it
//! to the coordinator's actor, which keeps it in [`Wakes`]. The actor sends what is waiting as one
//! turn, through the same resume as `agent/send`, once [`BATCH`] has passed since the first
//! summary arrived and its own CLI isn't running: a turn in progress gets them next. After
//! [`CAP`] wake-ups in a row with no message from the user, after the user stops the coordinator,
//! or when a wake-up can't start it, it pauses them until the user writes and reports
//! `agent.wakeupsPaused`.
//!
//! A restart keeps the count and a pause in the store (RYA-178). What was waiting, and the runs
//! the stop interrupted, [`catch_up`] rebuilds from the store when plxd starts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parallax_protocol::{AgentFailureKind, AgentOutcome, AgentRun, AgentStatus, RunId, TurnId};
use parallax_store::WakeState;
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

use super::actor::Command;
use super::convert::{INTERRUPTED, NO_WRITE, agent_run, option_name, truncate};
use super::{store, store_error};
use crate::server::Daemon;

/// How long wake-ups wait after the first arrives, so runs that finish together make one turn.
const BATCH: Duration = Duration::from_secs(2);

/// Wake-up turns a coordinator takes in a row, with no message from the user, before plxd
/// pauses them.
pub(super) const CAP: u32 = 10;

/// How much of a run's task and last message a summary quotes.
const TASK_BYTES: usize = 200;
pub(super) const EXCERPT_BYTES: usize = 500;

/// A coordinator's waiting wake-ups, how many it has taken since the user last wrote, and whether
/// they are paused. The actor stores `state` whenever it changes, so a restart keeps it.
#[derive(Debug, Default)]
pub(super) struct Wakes {
    waiting: Vec<String>,
    since: Option<Instant>,
    state: WakeState,
    /// The wake-up turn last sent, until its `turnStarted` is logged. Only one can be in flight,
    /// since none is sent while the coordinator's CLI runs.
    sent: Option<TurnId>,
}

impl Wakes {
    /// Adds a finished run's summary.
    pub fn push(&mut self, summary: String, now: Instant) {
        self.waiting.push(summary);
        self.since.get_or_insert(now);
    }

    /// When what is waiting is due, unless nothing is or wake-ups are paused.
    pub fn due(&self) -> Option<Instant> {
        self.since
            .filter(|_| !self.state.paused)
            .map(|since| since + BATCH)
    }

    /// The next wake-up turn's id and message, or `None` once the cap is reached. What is
    /// waiting stays until [`Wakes::delivered`].
    pub fn next(&mut self) -> Option<(TurnId, String)> {
        if self.state.in_a_row >= CAP {
            return None;
        }
        let turn = TurnId::generate();
        self.sent = Some(turn);
        Some((turn, message(&self.waiting)))
    }

    /// The wake-up from [`Wakes::next`] reached the coordinator's CLI: it counts against the cap,
    /// and what waited is gone.
    pub fn delivered(&mut self) {
        self.state.in_a_row += 1;
        self.waiting.clear();
        self.since = None;
    }

    /// The user wrote to the coordinator: the count starts over, and a pause ends. Whether that
    /// changed anything.
    pub fn attended(&mut self) -> bool {
        std::mem::take(&mut self.state) != WakeState::default()
    }

    /// Nothing wakes the coordinator until the user writes again. Whether it wasn't paused
    /// already.
    pub fn pause(&mut self) -> bool {
        !std::mem::replace(&mut self.state.paused, true)
    }

    /// The count and pause, as the store keeps them across a restart (RYA-178).
    pub fn state(&self) -> WakeState {
        self.state
    }

    /// Takes up the count and pause a previous plxd stored.
    pub fn restore(&mut self, state: WakeState) {
        self.state = state;
    }

    /// Drops what is waiting, for a coordinator a newer one replaced (0024).
    pub fn clear(&mut self) {
        self.waiting.clear();
        self.since = None;
    }

    /// Whether `turn` was sent as a wake-up, forgetting it.
    pub fn was_sent(&mut self, turn: TurnId) -> bool {
        self.sent.take_if(|sent| *sent == turn).is_some()
    }
}

/// Hands `summary` to coordinator `thread`'s actor, spawning one after a restart, without waiting
/// for it.
pub(super) fn notify(daemon: &Arc<Daemon>, thread: Uuid, summary: String) {
    let Ok(id) = RunId::try_from(thread) else {
        return;
    };
    let owned = Arc::clone(daemon);
    daemon.agents.tracker.spawn(async move {
        match super::actor_for(&owned, id).await {
            Ok(actor) => {
                // A closed channel is a coordinator that stopped or was deleted: nothing to wake.
                let _ = actor.send(Command::Wake(summary)).await;
            }
            Err(error) => {
                warn!(coordinator = %id, error = %error.message, "could not wake a coordinator");
            }
        }
    });
}

/// After a restart, hands each project's current coordinator one summary of the runs it started
/// that ended after its last turn began (RYA-178): the runs the stop interrupted, and any whose
/// wake-up was still waiting. A run a wake-up already named ended before that wake-up's turn, so
/// it isn't named again. If the coordinator's own turn was interrupted, the summary says so, since
/// nothing else would pick it back up. Called once at startup, after runs the store still has
/// running are marked interrupted.
// ponytail: rebuilt from run rows, so a summary lacks the run's last result, and a run that ended
// before the user's last message to the coordinator isn't named; store the summaries if that
// matters.
pub(super) async fn catch_up(daemon: &Arc<Daemon>) {
    let missed = store(daemon, |db| {
        let runs = db.list_runs(None).map_err(|e| store_error(&e))?;
        // Oldest first, so each project keeps its newest no-write run: its coordinator (0024).
        let coordinators: HashMap<Uuid, &parallax_store::Run> = runs
            .iter()
            .filter(|run| run.fields.policy == NO_WRITE)
            .map(|run| (run.fields.project_id, run))
            .collect();
        let mut missed = Vec::new();
        for coordinator in coordinators.into_values() {
            let since = db
                .last_turn_at(coordinator.id)
                .map_err(|e| store_error(&e))?
                .unwrap_or(coordinator.created_at);
            let mut lines = Vec::new();
            for run in runs.iter().filter(|run| {
                run.fields.coordinator_thread == Some(coordinator.id) && run.updated_at > since
            }) {
                if run.id == coordinator.id {
                    if run.state.status == INTERRUPTED {
                        lines.push(OWN_TURN.to_owned());
                    }
                    continue;
                }
                let worktree = db.get_worktree(run.id).map_err(|e| store_error(&e))?;
                if let Some(line) = agent_run(run, worktree.as_ref())
                    .ok()
                    .and_then(|run| stored_summary(&run))
                {
                    lines.push(line);
                }
            }
            if !lines.is_empty() {
                missed.push((coordinator.id, lines.join("\n")));
            }
        }
        Ok(missed)
    })
    .await;
    match missed {
        Ok(missed) => {
            for (thread, summary) in missed {
                info!(coordinator = %thread, "waking a coordinator for what it missed while plxd was stopped");
                notify(daemon, thread, summary);
            }
        }
        Err(error) => {
            warn!(error = %error.message, "could not find what coordinators missed while plxd was stopped");
        }
    }
}

/// The summary line for a coordinator whose own turn a stop interrupted.
const OWN_TURN: &str = "- Your own last turn was interrupted when plxd stopped; pick it back up.";

/// [`summary`] from `run`'s row alone, for a run whose wake-up a restart lost: the row keeps its
/// status and error, but not its last result or its failure's kind. `None` for a run that hasn't
/// ended.
fn stored_summary(run: &AgentRun) -> Option<String> {
    let outcome = match run.status {
        AgentStatus::Completed => AgentOutcome::Completed { result: None },
        AgentStatus::Failed => AgentOutcome::Failed {
            failure: AgentFailureKind::Unknown,
            message: run.error.clone().unwrap_or_default(),
        },
        AgentStatus::Cancelled => AgentOutcome::Cancelled,
        AgentStatus::Interrupted => AgentOutcome::Interrupted,
        _ => return None,
    };
    Some(summary(run, &outcome))
}

/// One line on how `run`'s CLI process ended: its id, task, outcome, and branch.
pub(super) fn summary(run: &AgentRun, outcome: &AgentOutcome) -> String {
    let ended = match outcome {
        AgentOutcome::Completed {
            result: Some(result),
        } => format!("completed, saying: {}", one_line(result, EXCERPT_BYTES)),
        AgentOutcome::Completed { result: None } => "completed".to_owned(),
        AgentOutcome::Failed {
            failure: AgentFailureKind::Unknown,
            message,
        } => format!("failed: {}", one_line(message, EXCERPT_BYTES)),
        AgentOutcome::Failed { failure, message } => format!(
            "failed ({}): {}",
            option_name(failure).unwrap_or_default(),
            one_line(message, EXCERPT_BYTES)
        ),
        AgentOutcome::Cancelled => "cancelled".to_owned(),
        AgentOutcome::Interrupted => {
            "interrupted when plxd stopped; message_agent resumes it".to_owned()
        }
        AgentOutcome::Unknown => "stopped".to_owned(),
    };
    let changes = match (&run.diff, &run.branch) {
        (Some(diff), Some(branch)) => format!(
            "Branch {branch} changes {} files (+{} -{}).",
            diff.files, diff.insertions, diff.deletions
        ),
        _ => "It has committed no changes.".to_owned(),
    };
    format!(
        "- Run {} ({}): {ended}. {changes}",
        run.id,
        task(&run.prompt)
    )
}

/// A run's task, as summaries and inbox items name it: the first line of its prompt that isn't
/// blank, cut short.
pub(super) fn task(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    one_line(line, TASK_BYTES)
}

/// `text` cut to about `max` bytes, on one line, so each run's summary stays one line.
pub(super) fn one_line(text: &str, max: usize) -> String {
    truncate(text, max)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A wake-up turn's message: the summaries, and what to do with them.
fn message(summaries: &[String]) -> String {
    format!(
        "Parallax, not the user: runs you started ended.\n\n{}\n\nReview them with agent_status \
         and agent_diff, message or start runs if more is needed, and tell the user where things \
         stand.",
        summaries.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;

    use super::{BATCH, CAP, Wakes};

    #[test]
    fn runs_finishing_together_make_one_turn_after_the_first_waits_its_batch() {
        let mut wakes = Wakes::default();
        assert_eq!(wakes.due(), None, "nothing waiting");
        let first = Instant::now();
        wakes.push("- Run a".to_owned(), first);
        wakes.push("- Run b".to_owned(), first + BATCH / 2);
        assert_eq!(
            wakes.due(),
            Some(first + BATCH),
            "the first one sets the time"
        );

        let (turn, message) = wakes.next().unwrap();
        assert!(message.contains("- Run a\n- Run b"), "{message}");
        assert_eq!(wakes.due(), Some(first + BATCH), "kept until delivered");
        wakes.delivered();
        assert_eq!(wakes.due(), None, "both went out in one turn");
        assert!(wakes.was_sent(turn));
        assert!(!wakes.was_sent(turn), "each turn is marked once");
    }

    #[test]
    fn the_cap_pauses_wake_ups_until_the_user_writes() {
        let mut wakes = Wakes::default();
        let now = Instant::now();
        for _ in 0..CAP {
            wakes.push("- Run".to_owned(), now);
            assert!(wakes.next().is_some());
            wakes.delivered();
        }
        wakes.push("- Run late".to_owned(), now);
        assert!(wakes.next().is_none(), "one past the cap");
        assert!(wakes.pause(), "which the actor pauses on, once");
        assert!(!wakes.pause());
        wakes.push("- Run later".to_owned(), now);
        assert_eq!(wakes.due(), None, "paused: nothing is due");

        wakes.attended();
        assert_eq!(wakes.due(), Some(now + BATCH), "what waited is due again");
        let (_, message) = wakes.next().unwrap();
        assert!(message.contains("late\n- Run later"), "{message}");
    }
}
