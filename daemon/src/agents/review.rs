//! `agent/diff` and `agent/file` (#157): what a reviewer sees of a run, read from git's objects
//! through the worktree's pinned git folder (#166). Both compare the run's latest commit, the one
//! `agent/accept` merges, with its worktree's base, so they never need the run's actor and never
//! touch the worktree's index or files while its agent runs.

use std::path::Path;
use std::sync::Arc;

use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AgentDiffFile, AgentDiffResult, AgentDiffStats, AgentFileParams, AgentFileResult,
    AgentFileSide, AgentFileStatus, ErrorKind, RunId,
};
use wisp_store::{Run as RunRow, Worktree};

use super::{convert, run_accepted, run_not_found, store, store_error};
use crate::server::Daemon;
use crate::worktree::{ChangeStatus, MAX_BLOB_BYTES, WorktreeError, validate_repo_path};

/// A run's row and worktree, if it can still be reviewed.
async fn reviewable(daemon: &Arc<Daemon>, id: RunId) -> Result<(RunRow, Worktree), ErrorObject> {
    let (row, worktree) = store(daemon, move |db| {
        let row = db
            .get_run(id.into())
            .map_err(|error| store_error(&error))?
            .ok_or_else(|| run_not_found(id))?;
        let worktree = db
            .get_worktree(id.into())
            .map_err(|error| store_error(&error))?;
        Ok((row, worktree))
    })
    .await?;
    if row.state.status == convert::ACCEPTED {
        return Err(run_accepted(id));
    }
    let Some(worktree) = worktree else {
        return Err(ErrorObject::internal_error(format!(
            "run {id} has no recorded worktree"
        )));
    };
    if worktree.git_dir.is_empty() {
        return Err(ErrorObject::wisp(
            ErrorKind::WorktreeFailed,
            format!(
                "run {id}'s worktree has no recorded git folder, so wispd can't read it safely"
            ),
        ));
    }
    Ok((row, worktree))
}

fn worktree_failed(error: &WorktreeError) -> ErrorObject {
    ErrorObject::wisp(ErrorKind::WorktreeFailed, error.to_string())
}

fn file_status(status: &ChangeStatus) -> AgentFileStatus {
    match status {
        ChangeStatus::Added => AgentFileStatus::Added,
        ChangeStatus::Modified => AgentFileStatus::Modified,
        ChangeStatus::Deleted => AgentFileStatus::Deleted,
        ChangeStatus::Renamed => AgentFileStatus::Renamed,
        ChangeStatus::Copied => AgentFileStatus::Copied,
        ChangeStatus::TypeChanged => AgentFileStatus::TypeChanged,
        ChangeStatus::Unmerged | ChangeStatus::Unknown(_) => AgentFileStatus::Unknown,
    }
}

/// `agent/diff`.
pub(crate) async fn diff(daemon: &Arc<Daemon>, id: RunId) -> Result<AgentDiffResult, ErrorObject> {
    let (row, worktree) = reviewable(daemon, id).await?;
    let head = row
        .state
        .commit_sha
        .clone()
        .unwrap_or_else(|| worktree.base.clone());
    let diff = daemon
        .agents
        .worktrees
        .diff_commits(
            Path::new(&worktree.path),
            Path::new(&worktree.git_dir),
            &worktree.base,
            &head,
        )
        .await
        .map_err(|error| worktree_failed(&error))?;
    let files = diff
        .files
        .into_iter()
        .map(|file| AgentDiffFile {
            status: file_status(&file.status),
            path: file.path,
            old_path: file.old_path,
            insertions: file.insertions,
            deletions: file.deletions,
            binary: file.binary,
            diff: file.diff,
            diff_truncated: file.diff_truncated,
        })
        .collect();
    Ok(AgentDiffResult {
        base: worktree.base,
        head,
        files,
        stats: AgentDiffStats {
            files: diff.stat.files,
            insertions: diff.stat.insertions,
            deletions: diff.stat.deletions,
        },
        truncated: diff.truncated,
    })
}

/// `agent/file`.
pub(crate) async fn file(
    daemon: &Arc<Daemon>,
    params: AgentFileParams,
) -> Result<AgentFileResult, ErrorObject> {
    let AgentFileParams {
        run_id,
        path,
        side,
        size_only,
    } = params;
    let size_only = size_only.unwrap_or(false);
    validate_repo_path(&path).map_err(ErrorObject::invalid_params)?;
    if side == AgentFileSide::Unknown {
        return Err(ErrorObject::invalid_params("side must be base or head"));
    }
    let (row, worktree) = reviewable(daemon, run_id).await?;
    let commit = match side {
        AgentFileSide::Head => row
            .state
            .commit_sha
            .clone()
            .unwrap_or_else(|| worktree.base.clone()),
        _ => worktree.base.clone(),
    };
    let blob = daemon
        .agents
        .worktrees
        .read_blob(
            Path::new(&worktree.path),
            Path::new(&worktree.git_dir),
            &commit,
            &path,
            if size_only { 0 } else { MAX_BLOB_BYTES },
        )
        .await
        .map_err(|error| worktree_failed(&error))?;
    Ok(match blob {
        None => AgentFileResult {
            path,
            side,
            commit,
            exists: false,
            size: None,
            content: None,
            too_large: false,
        },
        Some(blob) => AgentFileResult {
            path,
            side,
            commit,
            exists: true,
            size: Some(blob.size),
            too_large: blob.size > MAX_BLOB_BYTES,
            content: blob.content.as_deref().filter(|_| !size_only).map(base64),
        },
    })
}

/// Standard base64, with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output, "{input}");
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }
}
