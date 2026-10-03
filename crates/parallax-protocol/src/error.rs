use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ProtocolRange;
use crate::jsonrpc::{ErrorObject, PLX_ERROR};

/// What went wrong, in a Parallax error's `data.kind`. Receivers match on it, never on the message.
///
/// A newer plxd may send kinds that are not listed here. Treat those as unknown errors, so a
/// `switch` over this type must not end in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// A request came before `initialize`.
    NotInitialized,
    /// The client's protocol range does not overlap plxd's. Its `detail` is an
    /// `IncompatibleProtocolDetail` in every protocol version.
    IncompatibleProtocol,
    /// The events after the requested `seq` are gone or too many to replay. Reload the snapshots
    /// and subscribe again.
    ResyncRequired,
    /// No project has the given id.
    ProjectNotFound,
    /// No key account has the given id.
    AccountNotFound,
    /// The Keychain is locked, or access to an item was denied. Distinct from a bare internal
    /// error so the client can tell "locked" from "broken" (#117).
    KeychainUnavailable,
    /// A create reused an existing id with different params, or `project/start` named a new run
    /// while the project's coordinator is starting or running.
    IdConflict,
    /// No shared context file has the given path (#155).
    ContextNotFound,
    /// The content would be over `context/write`'s per-file or per-project size cap (#155).
    ContextTooLarge,
    /// A new project's `repoPath` is not the top folder of a git working tree on this host. The
    /// message says what is wrong with it.
    NotARepository,
    /// No agent run has the given id (#156).
    RunNotFound,
    /// `agent/send` can't resume the run: it ended before its CLI reported a session, or it is
    /// still starting.
    RunNotResumable,
    /// plxd won't start a worker as asked: its backend doesn't implement the worker sandbox
    /// (0013), the CLI is missing or older than the version the sandbox needs, or a path it would
    /// sandbox holds `*`, `?`, `[`, or `]`. The message says which, and for an old CLI names both
    /// versions. `project/start` fails with it too when the account's backend can't coordinate.
    WorkerUnavailable,
    /// plxd could not create the run's worktree, for example because the project's repository
    /// has uncommitted changes, or read the git state of the run's folder (RYA-298). The message
    /// says what to do.
    WorktreeFailed,
    /// The run was accepted (#157): its worktree and branch are gone, so there is nothing left to
    /// review, and it takes no more messages.
    RunAccepted,
    /// `agent/accept` refused before changing anything: the run is still running or has no
    /// commit, the repository's HEAD is detached or a merge or rebase is in progress there,
    /// uncommitted changes in the user's checkout touch files the merge would change, or the run
    /// has committed since the reviewed commit. The message says which.
    MergeRefused,
    /// `agent/accept` refused because the run's commit conflicts with the project's branch, which
    /// has moved on since the run started. The message names the conflicting files. Nothing was
    /// changed.
    MergeConflict,
    /// No repo entry has the given id (#110).
    RepoNotFound,
    /// No normal thread has the given run id (#110).
    ThreadNotFound,
    /// A run named no account, and its role has no default (0012). Set one with
    /// `accounts/defaults/set`, then retry with the same run id.
    NoDefaultAccount,
    /// The run's backend can't honor a `model`, `effort`, `permission`, `contextWindow`, or
    /// `fast` that `agent/start` or `thread/start` asked for, or the model's name can't be passed
    /// to its CLI (RYA-97). Nothing was created. The message names the option, the value, and the
    /// backend.
    UnsupportedOption,
    /// `agent/openPr` refused before pushing anything: the run is still running, it has no commit
    /// beyond its base, or it is a thread with no repo, which has no `origin` (RYA-168).
    PrRefused,
    /// `agent/openPr` could not push the run's branch: the repository has no `origin`, or git
    /// failed. The message carries git's stderr.
    PushFailed,
    /// `gh` isn't installed on the host, or isn't signed in. The message says which, with gh's
    /// stderr. For `agent/openPr`, the branch was pushed first.
    GhUnavailable,
    /// `gh` could not find or open the pull request, for example because `origin` isn't a GitHub
    /// repository, or `pr/view` or `pr/act` failed, as for a merge GitHub refuses (PLX-318). The
    /// message carries gh's stderr. For `agent/openPr`, the branch was pushed first.
    PrFailed,
    /// An image in `images` is over the per-image cap, or a message's images are over the
    /// per-message cap or count, which `promptImages`' options give (RYA-191). The message says
    /// which. Nothing was sent. `project/create`, `project/update`, and `repo/update` also return
    /// it when `icon.image` is over `iconImages`' `maxBytes` (PLX-339, 0038); nothing changed.
    ImageTooLarge,
    /// No image of the run has the given id (RYA-191).
    ImageNotFound,
    /// The run has no permission request with the given id, as a run started without
    /// `approvals` never has, or none this plxd has seen since it started (RYA-222).
    ApprovalNotFound,
    /// `agent/commit` or `agent/push` refused with nothing changed (RYA-298): the run is still
    /// running, there is nothing to commit, or its folder has a detached HEAD, with no branch to
    /// push. The message says which.
    GitRefused,
    /// `agent/commit`'s git failed, such as for a missing `user.name`. The message carries git's
    /// stderr.
    CommitFailed,
    /// The run's queue has no waiting message with the given id: it was sent, cancelled, or never
    /// queued (PLX-370).
    QueuedMessageNotFound,
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// The `data` of a Parallax error (code -32000).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    /// What went wrong.
    pub kind: ErrorKind,
    /// More about it, in a shape that depends on `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<Value>,
}

/// The `detail` of `incompatibleProtocol`. Its shape never changes, so every client can read it
/// from every plxd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleProtocolDetail {
    /// The versions the client asked for.
    pub requested: ProtocolRange,
    /// The versions plxd speaks.
    pub supported: ProtocolRange,
    /// plxd's release version, so the client can say which side to update.
    pub plxd: String,
}

impl ErrorObject {
    /// A Parallax error of `kind`, with no detail.
    pub fn parallax(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: PLX_ERROR,
            message: message.into(),
            data: serde_json::to_value(ErrorData { kind, detail: None }).ok(),
        }
    }

    /// The `incompatibleProtocol` answer to an `initialize` whose range does not overlap.
    #[must_use]
    pub fn incompatible_protocol(detail: &IncompatibleProtocolDetail) -> Self {
        let IncompatibleProtocolDetail {
            requested,
            supported,
            plxd,
        } = detail;
        let message = format!(
            "plxd {plxd} speaks protocol versions {} to {}, but the client asked for {} to {}",
            supported.min, supported.max, requested.min, requested.max,
        );
        let data = ErrorData {
            kind: ErrorKind::IncompatibleProtocol,
            detail: serde_json::to_value(detail).ok(),
        };
        Self {
            code: PLX_ERROR,
            message,
            data: serde_json::to_value(data).ok(),
        }
    }

    /// The error's data, when it is a Parallax error.
    #[must_use]
    pub fn parallax_data(&self) -> Option<ErrorData> {
        if self.code != PLX_ERROR {
            return None;
        }
        ErrorData::deserialize(self.data.as_ref()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ErrorData, ErrorKind, IncompatibleProtocolDetail};
    use crate::ProtocolRange;
    use crate::jsonrpc::{ErrorObject, INTERNAL_ERROR, PLX_ERROR};

    #[test]
    fn kinds_are_camel_case_strings() {
        for (kind, name) in [
            (ErrorKind::NotInitialized, "notInitialized"),
            (ErrorKind::IncompatibleProtocol, "incompatibleProtocol"),
            (ErrorKind::ResyncRequired, "resyncRequired"),
            (ErrorKind::ProjectNotFound, "projectNotFound"),
            (ErrorKind::AccountNotFound, "accountNotFound"),
            (ErrorKind::KeychainUnavailable, "keychainUnavailable"),
            (ErrorKind::IdConflict, "idConflict"),
            (ErrorKind::ContextNotFound, "contextNotFound"),
            (ErrorKind::ContextTooLarge, "contextTooLarge"),
            (ErrorKind::NotARepository, "notARepository"),
            (ErrorKind::RunNotFound, "runNotFound"),
            (ErrorKind::RunNotResumable, "runNotResumable"),
            (ErrorKind::WorkerUnavailable, "workerUnavailable"),
            (ErrorKind::WorktreeFailed, "worktreeFailed"),
            (ErrorKind::RunAccepted, "runAccepted"),
            (ErrorKind::MergeRefused, "mergeRefused"),
            (ErrorKind::MergeConflict, "mergeConflict"),
            (ErrorKind::RepoNotFound, "repoNotFound"),
            (ErrorKind::ThreadNotFound, "threadNotFound"),
            (ErrorKind::NoDefaultAccount, "noDefaultAccount"),
            (ErrorKind::UnsupportedOption, "unsupportedOption"),
            (ErrorKind::PrRefused, "prRefused"),
            (ErrorKind::PushFailed, "pushFailed"),
            (ErrorKind::GhUnavailable, "ghUnavailable"),
            (ErrorKind::PrFailed, "prFailed"),
            (ErrorKind::GitRefused, "gitRefused"),
            (ErrorKind::CommitFailed, "commitFailed"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<ErrorKind>(json!(name)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn unknown_kinds_decode_as_unknown() {
        let data: ErrorData =
            serde_json::from_value(json!({"kind": "planReplaced", "detail": {"planId": "p"}}))
                .unwrap();
        assert_eq!(data.kind, ErrorKind::Unknown);
    }

    #[test]
    fn parallax_errors_carry_their_kind_in_data() {
        let error = ErrorObject::parallax(ErrorKind::NotInitialized, "initialize first");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"code": -32000, "message": "initialize first", "data": {"kind": "notInitialized"}})
        );
        assert_eq!(
            error.parallax_data(),
            Some(ErrorData {
                kind: ErrorKind::NotInitialized,
                detail: None
            })
        );
        assert_eq!(ErrorObject::new(INTERNAL_ERROR, "x").parallax_data(), None);
        assert_eq!(ErrorObject::new(PLX_ERROR, "no data").parallax_data(), None);
    }

    #[test]
    fn incompatible_protocol_has_its_frozen_shape() {
        let detail = IncompatibleProtocolDetail {
            requested: ProtocolRange { min: 2, max: 3 },
            supported: ProtocolRange { min: 1, max: 1 },
            plxd: "0.1.0".to_owned(),
        };
        let error = ErrorObject::incompatible_protocol(&detail);
        assert_eq!(
            serde_json::to_value(&error).unwrap()["data"],
            json!({
                "kind": "incompatibleProtocol",
                "detail": {
                    "requested": {"min": 2, "max": 3},
                    "supported": {"min": 1, "max": 1},
                    "plxd": "0.1.0"
                }
            })
        );
        let data = error.parallax_data().unwrap();
        assert_eq!(
            serde_json::from_value::<IncompatibleProtocolDetail>(data.detail.unwrap()).unwrap(),
            detail
        );
        assert!(error.message.contains("0.1.0"), "{}", error.message);
    }
}
