//! A run's waiting messages (PLX-370, decision 0048): `queue/list`, `queue/edit`,
//! `queue/reorder`, `queue/cancel`, and `queue/steer`, behind the `queue` capability, and
//! `agent/send`'s `delivery`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{RunId, TurnId};

/// How `agent/send` delivers a message to a run whose CLI is working on a turn, behind the
/// `queue` capability. A run with no turn running takes either at once.
///
/// A newer peer may send a value this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentDelivery {
    /// The default: the message waits in the run's queue, stored, until the turn ends.
    #[default]
    Queue,
    /// The message goes into the turn that is running now. A steer can't change the run's model,
    /// options, or account.
    Steer,
    /// A value this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// A message waiting in a run's queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    /// The message's `turnId` from `agent/send`, which its turn keeps once it is sent.
    pub id: TurnId,
    /// The message.
    pub text: String,
    /// How many images go with it.
    pub images: u32,
    /// The threads attached to it (PLX-372), whose summaries the CLI gets with it.
    pub threads: Vec<RunId>,
}

/// Params of `queue/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueListParams {
    /// The run.
    pub run_id: RunId,
}

/// Params of `queue/edit`: replaces a waiting message's text, keeping its images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueEditParams {
    /// The run.
    pub run_id: RunId,
    /// The message.
    pub id: TurnId,
    /// Its new text, which may be empty only when it has images.
    pub text: String,
}

/// Params of `queue/reorder`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueReorderParams {
    /// The run.
    pub run_id: RunId,
    /// Every waiting message's id, in the new order, first to be sent first.
    pub ids: Vec<TurnId>,
}

/// Params of `queue/cancel`: drops a waiting message, which is never sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueCancelParams {
    /// The run.
    pub run_id: RunId,
    /// The message.
    pub id: TurnId,
}

/// Params of `queue/steer`: takes a waiting message out of the queue and sends it into the turn
/// running now, as `agent/send` with `delivery: steer` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueSteerParams {
    /// The run.
    pub run_id: RunId,
    /// The message.
    pub id: TurnId,
}

/// Result of every `queue/*` method: the run's queue after it, first to be sent first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueResult {
    /// The waiting messages.
    pub messages: Vec<QueuedMessage>,
}
