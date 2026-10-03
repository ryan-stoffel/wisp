//! A run's linked pull requests (PLX-318), behind the `pullRequests` capability.
//!
//! plxd links a pull request to a run when `agent/openPr` returns it, and when the run's agent
//! opens one itself with `gh pr create`; `AgentRun.pullRequests` lists them. `pr/view` reads one
//! from GitHub and `pr/act` merges, drafts, or closes it, each with `gh` on the host, as the user.
//! Behind the `prDiff` capability (PLX-328), `pr/diff` reads its unified diff. Each takes only a
//! URL linked to the run. Behind `threadTools` (0041), `pr/link` and `pr/unlink` add or remove a
//! GitHub pull request URL from a run's list, as a thread's Parallax tools do.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::RunId;

/// Params of `pr/view`, of `pr/diff`, and of `pr/link` and `pr/unlink`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrViewParams {
    /// The run.
    pub run_id: RunId,
    /// The pull request's web URL, as the run's `pullRequests` lists it.
    pub url: String,
}

/// What `pr/act` does to a pull request.
///
/// A newer client may send an action this version does not know; plxd refuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrAction {
    /// Merges it now with a merge commit.
    Merge,
    /// Squashes it into one commit and merges that now.
    Squash,
    /// Turns on auto-merge, with a merge commit, once its requirements pass.
    AutoMerge,
    /// Turns auto-merge off.
    DisableAutoMerge,
    /// Turns it back into a draft.
    Draft,
    /// Marks a draft ready for review.
    Ready,
    /// Closes it without merging.
    Close,
    /// An action this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Params of `pr/act`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrActParams {
    /// The run.
    pub run_id: RunId,
    /// The pull request's web URL, as the run's `pullRequests` lists it.
    pub url: String,
    /// What to do.
    pub action: PrAction,
}

/// Where a pull request is.
///
/// A newer plxd may send a state this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrState {
    /// Open.
    Open,
    /// Closed without merging.
    Closed,
    /// Merged.
    Merged,
    /// A state this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How a pull request merges.
///
/// A newer plxd may send a method this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrMergeMethod {
    /// With a merge commit.
    Merge,
    /// Squashed into one commit.
    Squash,
    /// Rebased onto the base branch.
    Rebase,
    /// A method this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Whether a pull request can merge, as GitHub's `mergeStateStatus` says.
///
/// A newer plxd may send a state this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrMergeState {
    /// It can merge.
    Clean,
    /// It can merge, but a check that isn't required failed.
    Unstable,
    /// It can merge, and the repository has pre-receive hooks.
    HasHooks,
    /// Its head branch is behind the base branch.
    Behind,
    /// A requirement blocks it, such as a review or a required check.
    Blocked,
    /// It conflicts with the base branch.
    Dirty,
    /// It is a draft.
    Draft,
    /// A state this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Where a check is, or all of a pull request's checks together.
///
/// A newer plxd may send a state this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrCheckState {
    /// Queued or running.
    Pending,
    /// It passed, or ended neutral.
    Passed,
    /// It failed, was cancelled, or timed out.
    Failed,
    /// It was skipped.
    Skipped,
    /// A state this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One check on a pull request's head commit: a GitHub Actions job or another CI's status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCheck {
    /// Its name.
    pub name: String,
    /// Where it is.
    pub state: PrCheckState,
    /// How it ended, as GitHub says it in lowercase, such as `success`, `failure`, or
    /// `timed_out`. Absent while it is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conclusion: Option<String>,
    /// Its page, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

/// A comment on a pull request, or a review's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrComment {
    /// Its author's login.
    pub author: String,
    /// Its Markdown.
    pub body: String,
    /// When it was written, in RFC 3339 UTC.
    pub created_at: Timestamp,
}

/// A review's verdict.
///
/// A newer plxd may send a verdict this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrReviewState {
    /// Approved.
    Approved,
    /// Asked for changes.
    ChangesRequested,
    /// Commented without a verdict.
    Commented,
    /// Dismissed.
    Dismissed,
    /// A verdict this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// A submitted review: who, what verdict, and when. Its text, if any, is also in `comments`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrReview {
    /// Its author's login.
    pub author: String,
    /// Its verdict.
    pub state: PrReviewState,
    /// When it was submitted, in RFC 3339 UTC.
    pub submitted_at: Timestamp,
}

/// A commit on a pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCommit {
    /// Its full hash.
    pub oid: String,
    /// Its message's first line.
    pub headline: String,
    /// Its first author's login, or name when they have no GitHub account.
    pub author: String,
    /// When it was committed, in RFC 3339 UTC.
    pub committed_at: Timestamp,
}

/// Result of `pr/diff`: a pull request's changes as one unified diff, as `gh pr diff` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrDiffResult {
    /// Its unified diff, a `diff --git` section per file.
    pub diff: String,
    /// Whether `diff` was cut short, at a line's end, at plxd's size cap.
    pub truncated: bool,
}

/// A pull request as GitHub has it now: the result of `pr/view` and `pr/act`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    /// Its number in its repository.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// Its web URL.
    pub url: String,
    /// Its repository, as `owner/name`.
    pub repo: String,
    /// Where it is.
    pub state: PrState,
    /// Whether it is a draft.
    pub draft: bool,
    /// Its author's login.
    pub author: String,
    /// When it last changed, in RFC 3339 UTC.
    pub updated_at: Timestamp,
    /// The branch it merges into.
    pub base_branch: String,
    /// The branch it merges from.
    pub head_branch: String,
    /// Files changed.
    pub changed_files: u64,
    /// Lines added.
    pub additions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// Its description, in Markdown.
    pub body: String,
    /// Its comments and the text of its reviews, oldest first.
    pub comments: Vec<PrComment>,
    /// Who is asked to review it: users' logins and teams' names.
    pub review_requests: Vec<String>,
    /// Its labels' names.
    pub labels: Vec<String>,
    /// The checks on its head commit.
    pub checks: Vec<PrCheck>,
    /// All its checks together: failed if any failed, else pending if any is, else passed.
    /// Absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checks_state: Option<PrCheckState>,
    /// When it was opened, in RFC 3339 UTC. Absent from a plxd without `prDiff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created_at: Option<Timestamp>,
    /// When it was closed or merged, in RFC 3339 UTC. Absent while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub closed_at: Option<Timestamp>,
    /// When it was merged, in RFC 3339 UTC. Absent unless it is merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_at: Option<Timestamp>,
    /// Who merged it. Absent unless it is merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_by: Option<String>,
    /// Its commits, oldest first. Absent means none, or a plxd without `prDiff`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<PrCommit>,
    /// Its submitted reviews, oldest first. Absent means none, or a plxd without `prDiff`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<PrReview>,
    /// Whether it can merge. Absent while GitHub is still working it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merge_state: Option<PrMergeState>,
    /// How it merges once its requirements pass, when auto-merge is on. Absent means off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_merge: Option<PrMergeMethod>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PrAction, PrCheckState, PrMergeMethod, PrMergeState, PrReviewState, PrState};

    #[test]
    fn unknown_values_decode_as_unknown() {
        assert_eq!(
            serde_json::from_value::<PrAction>(json!("rebase")).unwrap(),
            PrAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PrState>(json!("locked")).unwrap(),
            PrState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PrMergeMethod>(json!("fastForward")).unwrap(),
            PrMergeMethod::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PrMergeState>(json!("queued")).unwrap(),
            PrMergeState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PrCheckState>(json!("stale")).unwrap(),
            PrCheckState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PrReviewState>(json!("pending")).unwrap(),
            PrReviewState::Unknown
        );
    }
}
