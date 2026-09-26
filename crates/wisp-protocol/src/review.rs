//! Reviewing and accepting an agent run's changes (#157, #68), behind the `agents` and
//! `agentReview` capabilities.
//!
//! What a client reviews is exactly what `agent/accept` merges: the run's latest commit, the one
//! `agent.diffReady` reported, against the commit its worktree was created from. `agent/diff`
//! lists the files that differ, with a unified diff each. `agent/file` reads one file on either
//! side, so a client can show it in a diff editor, for a local or a remote host alike (#67).
//! `agent/accept` merges the run's commit into the project repository's current branch on the
//! host and removes the run's worktree and branch. `agent/requestChanges` sends the run a
//! follow-up, as `agent/send` does.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{RunId, TurnId};

uuid_v7_id! {
    /// An `agent/accept`'s id: a version 7 UUID that the client generates once and sends again on
    /// every retry, so a retry after a lost connection gets the same answer.
    AcceptId
}

/// Params of `agent/diff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiffParams {
    /// The run.
    pub run_id: RunId,
}

/// How a file differs from the base.
///
/// A newer wispd may send a status this version does not know; treat it as modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentFileStatus {
    /// Added: only the head side exists.
    Added,
    /// Modified.
    Modified,
    /// Deleted: only the base side exists.
    Deleted,
    /// Renamed from `oldPath`.
    Renamed,
    /// Copied from `oldPath`.
    Copied,
    /// Its type changed, for example a file became a symlink.
    TypeChanged,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One file that differs between the run's base and its latest commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiffFile {
    /// Its path on the head side, relative to the repository root. For a deleted file, its path
    /// on the base side.
    pub path: String,
    /// Its path on the base side, for a rename or a copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub old_path: Option<String>,
    /// How it changed.
    pub status: AgentFileStatus,
    /// Lines added. 0 for a binary file.
    pub insertions: u64,
    /// Lines removed. 0 for a binary file.
    pub deletions: u64,
    /// Whether git treats it as binary. A binary file has no `diff`.
    pub binary: bool,
    /// Its unified diff, starting at its `diff --git` line. Absent for a binary file, and for
    /// every file after the result's diffs reached their total size cap; read those files with
    /// `agent/file` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<String>,
    /// Whether `diff` was cut short at the per-file size cap.
    pub diff_truncated: bool,
}

/// Totals of an `agent/diff`, over every file, including files left out of `files`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiffStats {
    /// Files changed.
    pub files: u64,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
}

/// Result of `agent/diff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiffResult {
    /// The commit the run's worktree was created from: the `base` side.
    pub base: String,
    /// The run's latest commit: the `head` side, and what `agent/accept` merges. Equal to `base`
    /// until wispd has committed something for the run.
    pub head: String,
    /// The files that differ, ordered by path.
    pub files: Vec<AgentDiffFile>,
    /// Totals over every changed file.
    pub stats: AgentDiffStats,
    /// Whether `files` was cut short because the run changed more files than one answer lists.
    pub truncated: bool,
}

/// Which side of a run's diff to read.
///
/// A newer client may send a side this version does not know; wispd refuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentFileSide {
    /// The commit the run's worktree was created from.
    Base,
    /// The run's latest commit.
    Head,
    /// A side this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Params of `agent/file`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentFileParams {
    /// The run.
    pub run_id: RunId,
    /// The file's path, relative to the repository root, as `agent/diff` lists it: no leading
    /// `/`, no `.` or `..` or empty component, no backslash, and nothing under `.git`.
    pub path: String,
    /// Which side to read.
    pub side: AgentFileSide,
    /// Whether to leave the content out and answer only `exists` and `size`, as a file system's
    /// `stat` needs. Absent means false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size_only: Option<bool>,
}

/// Result of `agent/file`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentFileResult {
    /// The path as asked.
    pub path: String,
    /// The side as asked.
    pub side: AgentFileSide,
    /// The commit it was read from.
    pub commit: String,
    /// Whether the file exists on that side. An added file has no base side, and a deleted file
    /// no head side.
    pub exists: bool,
    /// Its size in bytes, when it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<u64>,
    /// Its exact content, base64-encoded, when it exists, is not too large, and `sizeOnly` was not
    /// asked. A symlink's
    /// content is its target, as git stores it; it is never followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content: Option<String>,
    /// Whether it is over `agent/file`'s size cap, so `content` is absent.
    pub too_large: bool,
}

/// How `agent/accept` brought a run's commit into the project's branch.
///
/// A newer wispd may send a value this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentMergeKind {
    /// The branch moved forward to the run's commit.
    FastForward,
    /// wispd made a merge commit, since the branch had moved on since the run started.
    Merge,
    /// The branch already contained the run's commit.
    UpToDate,
    /// A value this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// What `agent/accept` did to the project's repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentMerge {
    /// The commit the branch points at now: the run's commit, or wispd's merge commit.
    pub commit: String,
    /// The branch it went into, such as `main`: the repository's current branch when the run
    /// was accepted.
    pub into: String,
    /// How.
    pub how: AgentMergeKind,
}

/// Params of `agent/accept`.
///
/// Idempotent on `id`: accepting an accepted run again with the same id returns the same result;
/// with another id it fails with `runAccepted`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentAcceptParams {
    /// The run.
    pub run_id: RunId,
    /// The accept's id, a version 7 UUID generated by the client.
    pub id: AcceptId,
    /// The commit the user reviewed, `agent/diff`'s `head`. When it is given and the run has
    /// committed since, the accept fails with `mergeRefused` instead of merging changes nobody
    /// reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub commit: Option<String>,
}

/// Result of `agent/accept`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentAcceptResult {
    /// The run, now `accepted`.
    pub run: crate::AgentRun,
    /// What happened to the project's repository.
    pub merge: AgentMerge,
}

/// Params of `agent/requestChanges`: the reviewer's follow-up to a run, sent to it as
/// `agent/send` sends a message, and idempotent on `turnId` the same way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequestChangesParams {
    /// The run.
    pub run_id: RunId,
    /// The message's id, a version 7 UUID generated by the client.
    pub turn_id: TurnId,
    /// What to change.
    pub text: String,
}
