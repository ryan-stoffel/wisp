use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{AccountChoice, AgentEffort, AgentPermission, PromptImage, RunId};

uuid_v7_id! {
    /// A project's id: a version 7 UUID that the client generates once and sends again on every
    /// retry of `project/create`.
    ProjectId
}

/// A project: a body of work on one repository, run by this plxd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// The id the client chose when it created the project.
    pub id: ProjectId,
    /// The name shown in the app.
    pub name: String,
    /// The icon the user chose, behind the `projectEdit` capability (RYA-227, 0032). Absent means
    /// the app's default icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<ProjectIcon>,
    /// The absolute path of the repository on this host.
    pub repo_path: String,
    /// The branch checked out in the repository, or the first 7 digits of the commit when `HEAD`
    /// is detached, read when plxd sends the project. Absent when plxd can't read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    /// The run of the project's coordinator chat, the newest one `project/start` started (0024).
    /// Its transcript, messages, and Stop go through `agent/*` like any run's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator: Option<RunId>,
    /// The permission mode its coordinator and every run in it start in (0042), behind the
    /// `projectPermission` capability. Absent only from an older plxd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<ProjectPermission>,
    /// When the project was created, in RFC 3339 UTC.
    pub created_at: Timestamp,
    /// When the project last changed, in RFC 3339 UTC. `project/update` leaves it as it is, since
    /// a rename or a new icon is not activity (0032).
    pub updated_at: Timestamp,
}

/// A project's permission mode (0042): the mode its coordinator and every run in it start in.
/// Each maps to the [`AgentPermission`] of the same name. A run on a backend that doesn't map it
/// is refused with `unsupportedOption`, never moved to another mode.
///
/// A newer plxd may send a value this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ProjectPermission {
    /// Auto: a classifier approves or blocks each action.
    Auto,
    /// Bypass Permissions: no permission checks.
    Bypass,
    /// A value this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

impl ProjectPermission {
    /// The run mode it maps to, or `None` for [`ProjectPermission::Unknown`].
    #[must_use]
    pub fn agent(self) -> Option<AgentPermission> {
        match self {
            Self::Auto => Some(AgentPermission::Auto),
            Self::Bypass => Some(AgentPermission::Bypass),
            Self::Unknown => None,
        }
    }
}

/// A project's icon (RYA-227, 0032): a Lucide icon and a color from the app's palette, both by
/// name, and optionally an uploaded image (PLX-339, 0038). plxd stores them as the client sent
/// them and never reads them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIcon {
    /// The Lucide icon's name in kebab-case, such as `rocket`: 1 to 64 characters of `a-z`, `0-9`,
    /// and `-`.
    pub name: String,
    /// The palette key of its color, such as `green`: 1 to 32 characters of `a-z`, `0-9`, and
    /// `-`. Absent means the app's accent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// An uploaded image the app draws instead of the glyph, behind the `iconImages` capability
    /// (0038). Its `data` is at most the capability's `maxBytes` of base64, or the request fails
    /// with `imageTooLarge`. Absent means no image, so an icon sent without one clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image: Option<PromptImage>,
}

/// Params of `project/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListParams {}

/// Result of `project/list`: a snapshot of every project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListResult {
    /// Every project, oldest first.
    pub projects: Vec<Project>,
    /// The `seq` of the last event the snapshot reflects. Subscribe with `after` set to it.
    pub seq: u64,
}

/// Params of `project/create`.
///
/// It is idempotent on `id`: if a project with that id exists, plxd returns it instead of
/// creating another, and fails with `idConflict` if `name`, `repoPath`, `icon`, or `permission`
/// differ. A new project's `repoPath` must be the top folder of a git working tree on this host,
/// or it fails with `notARepository`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateParams {
    /// The new project's id, a version 7 UUID generated by the client.
    pub id: ProjectId,
    /// The name shown in the app.
    pub name: String,
    /// The absolute path of the repository on this host.
    pub repo_path: String,
    /// The project's icon, sent only to a plxd that advertises `projectEdit`. Absent means the
    /// app's default icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<ProjectIcon>,
    /// The project's permission mode, sent only to a plxd that advertises `projectPermission`.
    /// Absent means `auto`, the mode projects from before it have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<ProjectPermission>,
}

/// Result of `project/create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateResult {
    /// The project, new or existing.
    pub project: Project,
}

/// Params of `project/update`: renames a project or sets its icon, behind the `projectEdit`
/// capability (RYA-227, 0032), or its permission mode, behind `projectPermission` (0042).
///
/// A field that is absent stays as it is, and `icon` replaces the whole icon. `name` follows
/// `project/create`'s rules, and the repository can't change. A rename, a new icon, or a new mode
/// is not activity, so `updatedAt` stays as it is. Fails with `projectNotFound` for an unknown project.
/// A change appends `project.updated`; an update that changes nothing appends no event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdateParams {
    /// The project.
    pub project: ProjectId,
    /// The new name. Absent keeps the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// The new icon. Absent keeps the icon, and so does `null`: an icon can't be removed (0032).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<ProjectIcon>,
    /// The new permission mode. Absent keeps the mode. Each run in the project starts its next
    /// CLI process in it, and a running CLI keeps its mode until it exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<ProjectPermission>,
}

/// Result of `project/update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdateResult {
    /// The project as it stands after the update.
    pub project: Project,
}

/// Params of `project/start`: starts the project's coordinator chat (0024), behind the
/// `coordinator` capability.
///
/// The coordinator is a run with policy `noWrite` in the project's repository, whose
/// `coordinatorThread` is its own id. Later messages, Stop, and its transcript go through
/// `agent/send`, `agent/cancel`, and `agent/events`, and its events are the project's `agent.*`
/// events. Idempotent on `runId` like `agent/start`: the same params return the run, and
/// different ones fail with `idConflict`. A project's coordinator is its newest one, which
/// `Project.coordinator` names. A new `runId` starts over: it replaces the coordinator unless that
/// one is starting or running, when it fails with `idConflict`. So a coordinator whose session
/// can't be resumed never locks its project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStartParams {
    /// The project.
    pub project: ProjectId,
    /// The coordinator run's id, a version 7 UUID generated by the client.
    pub run_id: RunId,
    /// The user's first message.
    pub prompt: String,
    /// The account to run on. Absent means the coordinator role's default
    /// (`accounts/defaults/*`). Only a backend that can coordinate takes it: Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
    /// The model, as `agent/start`'s. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// How hard the model thinks, as `agent/start`'s. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// Ignored: the coordinator runs in the project's permission mode (0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// Images for the first message, as `agent/start`'s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
    /// Forward the coordinator's permission requests to the client, as `agent/start` takes it.
    /// The runs it spawns forward theirs too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approvals: bool,
}

/// Params of `project/delete`: deletes a project with its coordinator and every run in it, their
/// stored events, sent turns, images, worktrees, and branches, and its shared context folder,
/// behind the `projectDelete` capability (PLX-338).
///
/// Running CLIs are cancelled first, and the delete answers once they have exited and the
/// project is gone, after appending `project.deleted`. Deleting a project that doesn't exist, or
/// a repo entry's id, fails with `projectNotFound`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDeleteParams {
    /// The project.
    pub project: ProjectId,
}

/// Result of `project/delete`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDeleteResult {}
