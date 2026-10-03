//! A Project's inbox (PLX-401, decision 0043), behind the `inbox` capability: what its children
//! did while the user was away, built from the events plxd handles. `inbox/list` lists it,
//! `inbox/seen` marks items read, and each new item appends `inbox.added` to the Project's events.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{ProjectId, RunId};

uuid_v7_id! {
    /// Identifies one inbox item. plxd generates it.
    InboxItemId
}

/// What an inbox item says, by 0043's kinds.
///
/// A newer plxd may send a kind this version does not know; treat it as unknown, and don't end
/// a `switch` over this type in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum InboxKind {
    /// Something waits on the user: a child's permission request, or paused wake-ups.
    NeedsYou,
    /// A child finished.
    Done,
    /// A child failed.
    Failed,
    /// The coordinator answered a question for the user.
    Decided,
    /// The Project's memory changed.
    Learned,
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One item of a Project's inbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    /// The item's id.
    pub id: InboxItemId,
    /// What it says.
    pub kind: InboxKind,
    /// The run it is about: a child, or the coordinator when its wake-ups paused.
    pub run: RunId,
    /// One line for people, such as the child's task and its diff stats.
    pub text: String,
    /// When plxd added it, in RFC 3339 UTC.
    pub created_at: Timestamp,
    /// When a client first marked it seen with `inbox/seen` (0033's read model). Absent if never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seen_at: Option<Timestamp>,
}

/// Params of `inbox/list`. Fails with `projectNotFound` for an unknown project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InboxListParams {
    /// The Project.
    pub project: ProjectId,
}

/// Result of `inbox/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InboxListResult {
    /// Every item, oldest first.
    pub items: Vec<InboxItem>,
    /// The `seq` of the last event the list reflects. Subscribe to the Project's events with
    /// `after` set to it for the items added since.
    pub seq: u64,
}

/// Params of `inbox/seen`: marks items seen at plxd's clock now. An item already seen keeps its
/// first `seenAt`, and ids not in the Project's inbox are skipped. Fails with `projectNotFound`
/// for an unknown project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InboxSeenParams {
    /// The Project.
    pub project: ProjectId,
    /// The items to mark.
    pub items: Vec<InboxItemId>,
}

/// Result of `inbox/seen`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InboxSeenResult {
    /// The marked items as they stand, oldest first.
    pub items: Vec<InboxItem>,
}
