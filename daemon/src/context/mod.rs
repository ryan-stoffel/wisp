//! Shared context: a folder per project, outside its git repository, that every agent and the
//! editor read and write.
//!
//! Layout: [`crate::paths::DataDir::context_dir`] under the data folder, one file deep. A path is
//! always exactly one file name relative to that folder, ending in `.md`, `.markdown`, or `.txt`.
//! Keeping it flat removes an entire class of intermediate-directory symlink attacks.
//!
//! Two more defenses hold even when a path passes that check:
//!
//! - Reads open with `O_NOFOLLOW`, so a symlink swapped in after validation is refused atomically.
//! - Writes go to a temporary file in the same folder, then `rename` it over the target.
//!   `rename` never follows a symlink at the destination, so even a swapped-in symlink can't be
//!   written through; an existing symlink is also rejected outright first, for a clear error.
//!
//! [`ContextIndex`] remembers, in memory, the last writer and content hash wispd has seen for
//! each file. It resets on restart: the file on disk is the source of truth, and this is only
//! bookkeeping for idempotent retries and the `lastWriter` display field.

pub(crate) mod watcher;

use std::collections::HashMap;
use std::io::{self, Read, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use jiff::Timestamp;
use rustix::fs::OFlags;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{ContextFile, ContextWriteId, ErrorKind, ProjectId};

use crate::paths::DataDir;

/// True if `error` is `ELOOP`, "too many levels of symbolic links": what `O_NOFOLLOW` produces
/// when the final path component is a symlink. `std::io::ErrorKind` has no stable variant for it
/// (`io_error_more` is still unstable), so this compares the raw OS error code instead.
fn is_symlink_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}

/// The largest a single shared context file may be: 1 MiB.
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The largest a project's shared context folder may total: 20 MiB.
pub(crate) const MAX_PROJECT_BYTES: u64 = 20 * MAX_FILE_BYTES;

const ALLOWED_EXTENSIONS: [&str; 3] = ["md", "markdown", "txt"];

/// Creates a project's shared context folder if it does not exist yet, private like the data
/// folder itself. Safe to call on every access: `project/create` calls it eagerly and best
/// effort, and every `context/*` call calls it again lazily, so an existing project that predates
/// this feature still gets one.
pub(crate) fn ensure_dir(data_dir: &DataDir, project: ProjectId) -> io::Result<PathBuf> {
    let dir = data_dir.context_dir(project);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Rejects `path` unless it is exactly one normal, non-hidden file name ending in `.md`,
/// `.markdown`, or `.txt`. This alone rejects `..`, a leading `/`, and any subdirectory, since all
/// of those need more than one path component or a component that is not [`Component::Normal`].
///
/// Returns the same string back.
pub(crate) fn validate_relative_path(path: &str) -> Result<&str, ErrorObject> {
    let invalid = || {
        ErrorObject::invalid_params(
            "path must be a single relative file name, with no \"..\", no leading \"/\", no \
             subdirectory, and no hidden (dot) name, ending in .md, .markdown, or .txt",
        )
    };
    if path.is_empty() || path.len() > 255 || path.contains('\0') {
        return Err(invalid());
    }
    let mut components = Path::new(path).components();
    let Some(Component::Normal(name)) = components.next() else {
        return Err(invalid());
    };
    // Comparing back to `path` also rejects a name `components` normalized (a trailing `/`).
    let extension = Path::new(path).extension().and_then(|ext| ext.to_str());
    if components.next().is_some()
        || name.to_str() != Some(path)
        || path.starts_with('.')
        || !extension.is_some_and(|ext| ALLOWED_EXTENSIONS.contains(&ext))
    {
        return Err(invalid());
    }
    Ok(path)
}

/// Reads a shared context file's content and metadata.
///
/// Opens with `O_NOFOLLOW`, so a symlink at `name` is refused rather than followed, whether or not
/// it was already there when the caller last checked.
///
/// # Errors
///
/// An [`io::Error`] of kind [`io::ErrorKind::NotFound`] if there is no such file (also raised for
/// a directory, so one can't be read as though it were content), or an `ELOOP` error (see
/// [`is_symlink_error`]) if it is a symlink.
pub(crate) fn read_file(dir: &Path, name: &str) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NOFOLLOW.bits().cast_signed())
        .open(dir.join(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let mut content = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut content)?;
    Ok((content, metadata))
}

/// Writes `content` to `name` in `dir`, replacing it in full.
///
/// Writes to a temporary file in `dir` first, then renames it over `name`
/// ([`tempfile::NamedTempFile::persist`]). `rename` replaces whatever is at the destination
/// without following it, so even a symlink swapped in after an earlier check is never written
/// through; a symlink already there is rejected outright first, for a clearer error in the
/// ordinary case where nothing is racing this call.
///
/// # Errors
///
/// An `ELOOP` [`io::Error`] (see [`is_symlink_error`]) if `name` is already a symlink, or another
/// [`io::Error`] from creating or renaming the temporary file.
pub(crate) fn write_file(dir: &Path, name: &str, content: &[u8]) -> io::Result<std::fs::Metadata> {
    let target = dir.join(name);
    if let Ok(metadata) = std::fs::symlink_metadata(&target)
        && metadata.file_type().is_symlink()
    {
        return Err(io::Error::from_raw_os_error(
            rustix::io::Errno::LOOP.raw_os_error(),
        ));
    }
    let mut temp = tempfile::Builder::new()
        .prefix(".wisp-context-")
        .tempfile_in(dir)?;
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    let file = temp.persist(&target).map_err(|error| error.error)?;
    file.metadata()
}

/// Lists the shared context files in `dir`, ordered by path.
///
/// A directory, a hidden (dot) entry (including wispd's own temporary files while a write is in
/// progress), a symlink, or a name with a disallowed extension is skipped rather than listed:
/// [`std::fs::DirEntry::file_type`] does not follow a symlink, so one is reported as a symlink,
/// never as whatever it points to.
pub(crate) fn list_files(dir: &Path) -> io::Result<Vec<(String, std::fs::Metadata)>> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if validate_relative_path(&name).is_err() {
            continue;
        }
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry.metadata()?;
        files.push((name, metadata));
    }
    files.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(files)
}

/// The total size of a project's shared context, in bytes, apart from `except`'s own current
/// size, if it exists. Used to check the per-project cap before a write, without counting the
/// file the write is about to replace against itself.
pub(crate) fn other_files_total(dir: &Path, except: &str) -> io::Result<u64> {
    Ok(list_files(dir)?
        .iter()
        .filter(|(name, _)| name != except)
        .map(|(_, metadata)| metadata.len())
        .sum())
}

fn hash_content(content: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// What [`ContextIndex`] remembers about the last write to one file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    /// The write that produced the current content, if it came through `context/write`. `None`
    /// for a write an agent made directly on disk.
    write_id: Option<ContextWriteId>,
    hash: u64,
    writer: Option<String>,
}

/// What an idempotent `context/write` found already recorded for its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Existing {
    /// No earlier write recorded, or the last one used a different id: proceed as a new write.
    New,
    /// The last write used this same id and had the same content and writer: a retry. Return the
    /// current file instead of writing again.
    SameRetry,
    /// The last write used this same id but different content or a different writer: `idConflict`.
    Conflict,
}

/// In-memory bookkeeping for shared context writes (see the module docs for why it is not
/// persisted). Keyed by project and path; safe to call from the watcher's own thread and from the
/// async method handlers alike, like [`crate::event_log::EventLog`].
#[derive(Default)]
pub(crate) struct ContextIndex {
    records: Mutex<HashMap<(ProjectId, String), Record>>,
}

impl ContextIndex {
    /// Whether `write_id` for `project`/`path` is a fresh write, a matching retry, or a conflict
    /// with different content or a different writer.
    pub fn check(
        &self,
        project: ProjectId,
        path: &str,
        write_id: ContextWriteId,
        content: &[u8],
        writer: Option<&str>,
    ) -> Existing {
        let records = self.lock();
        let Some(record) = records.get(&(project, path.to_owned())) else {
            return Existing::New;
        };
        if record.write_id != Some(write_id) {
            return Existing::New;
        }
        if record.hash == hash_content(content) && record.writer.as_deref() == writer {
            Existing::SameRetry
        } else {
            Existing::Conflict
        }
    }

    /// Records a write that came through `context/write`.
    pub fn record_protocol_write(
        &self,
        project: ProjectId,
        path: &str,
        write_id: ContextWriteId,
        writer: Option<String>,
        content: &[u8],
    ) {
        self.lock().insert(
            (project, path.to_owned()),
            Record {
                write_id: Some(write_id),
                hash: hash_content(content),
                writer,
            },
        );
    }

    /// True if `content`'s hash already matches the last write recorded for this path, whether
    /// that was `context/write`'s own or an earlier disk observation. A filesystem event that
    /// matches carries no new information and should not be reported: it is either the echo of
    /// wispd's own write (which the OS can report more than once for a single rename, so this
    /// checks content rather than consuming a one-shot flag), or a rewrite of a file with the
    /// content it already had.
    pub fn matches_recorded(&self, project: ProjectId, path: &str, content: &[u8]) -> bool {
        self.lock()
            .get(&(project, path.to_owned()))
            .is_some_and(|record| record.hash == hash_content(content))
    }

    /// Records a write the watcher found on disk, with no `context/write` behind it.
    pub fn record_external_write(&self, project: ProjectId, path: &str, content: &[u8]) {
        self.lock().insert(
            (project, path.to_owned()),
            Record {
                write_id: None,
                hash: hash_content(content),
                writer: None,
            },
        );
    }

    /// Who last wrote `path` in `project`, if wispd has seen a write to it since it started.
    pub fn writer_of(&self, project: ProjectId, path: &str) -> Option<String> {
        self.lock()
            .get(&(project, path.to_owned()))
            .and_then(|record| record.writer.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(ProjectId, String), Record>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `metadata` and `writer` as the protocol's [`ContextFile`].
pub(crate) fn context_file(
    path: &str,
    metadata: &std::fs::Metadata,
    writer: Option<String>,
) -> ContextFile {
    ContextFile {
        path: path.to_owned(),
        size: metadata.len(),
        modified_at: Timestamp::new(
            metadata.mtime(),
            metadata.mtime_nsec().try_into().unwrap_or(0),
        )
        .unwrap_or(Timestamp::UNIX_EPOCH),
        last_writer: writer,
    }
}

/// The protocol error for a shared context I/O failure.
pub(crate) fn io_error(path: &str, error: &io::Error) -> ErrorObject {
    if error.kind() == io::ErrorKind::NotFound {
        return ErrorObject::wisp(
            ErrorKind::ContextNotFound,
            format!("no shared context file has path {path}"),
        );
    }
    if is_symlink_error(error) {
        return ErrorObject::invalid_params(format!("{path} is a symlink, which is not allowed"));
    }
    ErrorObject::internal_error(format!("shared context I/O failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use wisp_protocol::jsonrpc::INVALID_PARAMS;

    use super::{
        ContextIndex, Existing, ensure_dir, list_files, other_files_total, read_file,
        validate_relative_path, write_file,
    };
    use crate::paths::DataDir;

    #[test]
    fn only_a_single_normal_markdown_or_text_name_is_valid() {
        for good in ["notes.md", "research.markdown", "todo.txt"] {
            assert_eq!(validate_relative_path(good), Ok(good), "{good}");
        }
        for bad in [
            "",
            "../notes.md",
            "/etc/notes.md",
            "sub/notes.md",
            ".hidden.md",
            "notes",
            "notes.png",
            "..",
            ".",
        ] {
            let error = validate_relative_path(bad).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn a_context_dir_is_created_private_and_ensuring_it_again_is_a_no_op() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(temp.path()).unwrap();
        let project = wisp_protocol::ProjectId::generate();
        let dir = ensure_dir(&data_dir, project).unwrap();
        assert!(dir.is_dir());
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(ensure_dir(&data_dir, project).unwrap(), dir);
    }

    #[test]
    fn write_then_read_round_trips_and_a_second_write_replaces_the_first() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "notes.md", b"first, and then some").unwrap();
        write_file(dir.path(), "notes.md", b"hello").unwrap();
        let (content, metadata) = read_file(dir.path(), "notes.md").unwrap();
        assert_eq!(content, b"hello");
        assert_eq!(metadata.len(), 5);

        let error = read_file(dir.path(), "missing.md").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_symlinked_target_is_refused_for_both_read_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "secret").unwrap();
        let link = dir.path().join("notes.md");
        symlink(&outside, &link).unwrap();

        let read_error = read_file(dir.path(), "notes.md").unwrap_err();
        assert!(super::is_symlink_error(&read_error));

        let write_error = write_file(dir.path(), "notes.md", b"clobbered").unwrap_err();
        assert!(super::is_symlink_error(&write_error));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
    }

    #[test]
    fn a_written_file_is_listed_and_a_temporary_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "notes.md", b"hello").unwrap();
        let leftover = tempfile::Builder::new()
            .prefix(".wisp-context-")
            .tempfile_in(dir.path())
            .unwrap();
        leftover.keep().unwrap();
        let files = list_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "notes.md");
    }

    #[test]
    fn other_files_total_excludes_the_named_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.md", &[b'a'; 100]).unwrap();
        write_file(dir.path(), "b.md", &[b'b'; 50]).unwrap();
        assert_eq!(other_files_total(dir.path(), "a.md").unwrap(), 50);
        assert_eq!(other_files_total(dir.path(), "b.md").unwrap(), 100);
        assert_eq!(other_files_total(dir.path(), "c.md").unwrap(), 150);
    }

    #[test]
    fn listing_a_context_dir_that_does_not_exist_yet_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_files(&dir.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn the_index_tells_new_writes_from_retries_and_conflicts() {
        let index = ContextIndex::default();
        let project = wisp_protocol::ProjectId::generate();
        let id = wisp_protocol::ContextWriteId::generate();
        assert_eq!(
            index.check(project, "notes.md", id, b"hello", None),
            Existing::New
        );

        index.record_protocol_write(project, "notes.md", id, Some("editor".to_owned()), b"hello");
        assert_eq!(
            index.check(project, "notes.md", id, b"hello", Some("editor")),
            Existing::SameRetry
        );
        assert_eq!(
            index.check(project, "notes.md", id, b"different", Some("editor")),
            Existing::Conflict
        );
        assert_eq!(
            index.check(project, "notes.md", id, b"hello", None),
            Existing::Conflict,
            "a different writer with the same id and content is still a conflict"
        );

        let other_id = wisp_protocol::ContextWriteId::generate();
        assert_eq!(
            index.check(project, "notes.md", other_id, b"anything", None),
            Existing::New,
            "a fresh id is always a new write, never a conflict"
        );
    }

    #[test]
    fn matching_content_is_recognized_no_matter_how_many_times_it_is_observed() {
        let index = ContextIndex::default();
        let project = wisp_protocol::ProjectId::generate();
        let id = wisp_protocol::ContextWriteId::generate();
        index.record_protocol_write(project, "notes.md", id, None, b"hello");
        // The OS can report more than one event for a single rename: each must still match.
        assert!(index.matches_recorded(project, "notes.md", b"hello"));
        assert!(index.matches_recorded(project, "notes.md", b"hello"));
        assert!(
            !index.matches_recorded(project, "notes.md", b"something else"),
            "different content is new information, not an echo"
        );
    }

    #[test]
    fn writer_of_reports_the_last_recorded_writer() {
        let index = ContextIndex::default();
        let project = wisp_protocol::ProjectId::generate();
        assert_eq!(index.writer_of(project, "notes.md"), None);
        index.record_external_write(project, "notes.md", b"from disk");
        assert_eq!(index.writer_of(project, "notes.md"), None);
        let id = wisp_protocol::ContextWriteId::generate();
        index.record_protocol_write(project, "notes.md", id, Some("editor".to_owned()), b"hi");
        assert_eq!(
            index.writer_of(project, "notes.md"),
            Some("editor".to_owned())
        );
    }
}
