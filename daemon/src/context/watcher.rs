//! Turns an agent's own writes to shared context, made directly on disk outside any wispd call,
//! into `context.changed` events (0005, #155).
//!
//! wispd watches with the `notify` crate (`FSEvents` on macOS) rather than rescanning the folder
//! on a timer. Nothing else would ever see an agent's write: it never goes through wispd at all,
//! so there is no request to hang the check off. A poll loop would then have to choose between
//! missing a quick write between ticks and burning cycles ticking often enough not to, while
//! `FSEvents` already tells the kernel's own record of every write to the tree, for free. One
//! watcher covers the whole `context/` folder, recursively, from before any project exists, so a
//! project created later needs no watch of its own: `FSEvents` reports changes anywhere under the
//! root it was given, including in a subfolder created after the watch started.

use std::path::{Component, Path};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::warn;
use wisp_protocol::ProjectId;

use super::{MAX_PROJECT_BYTES, context_file, validate_relative_path};
use crate::server::Daemon;

/// Starts watching `daemon`'s shared context folder. The returned watcher must be kept alive for
/// as long as wispd should keep reporting changes; dropping it stops the watch.
///
/// # Errors
///
/// If the platform's watcher backend could not be started or could not watch the folder.
pub(crate) fn start(daemon: std::sync::Arc<Daemon>) -> notify::Result<RecommendedWatcher> {
    let root = daemon.data_dir.context_root();
    // FSEvents reports its own canonical form of a path, which on macOS can differ from wispd's
    // own spelling (`/tmp/...` versus the real `/private/tmp/...`, for example). Canonicalizing
    // once here, rather than comparing against `root` as wispd spelled it, keeps `relative_file`
    // matching every event instead of silently matching none of them. `context_root` is created
    // just before this is called (`Server::start`), so canonicalizing it should not fail; if it
    // somehow does, falling back to `root` at least keeps the watch itself running.
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<Event>| match event {
            Ok(event) => handle(&daemon, &canonical_root, &event),
            Err(error) => warn!(%error, "the shared context watcher failed"),
        })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

fn handle(daemon: &Daemon, context_root: &Path, event: &Event) {
    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
        return;
    }
    for path in &event.paths {
        observe(daemon, context_root, path);
    }
}

/// The project and file name a changed path names, if it is exactly one file directly inside one
/// project's context folder. Anything else (the project folder itself, something nested deeper,
/// or a folder name that isn't a project id) is not a shared context file and is ignored.
fn relative_file(context_root: &Path, path: &Path) -> Option<(ProjectId, String)> {
    let relative = path.strip_prefix(context_root).ok()?;
    let mut components = relative.components();
    let Component::Normal(project_name) = components.next()? else {
        return None;
    };
    let project: ProjectId = project_name.to_str()?.parse().ok()?;
    let Component::Normal(file_name) = components.next()? else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }
    Some((project, file_name.to_str()?.to_owned()))
}

fn observe(daemon: &Daemon, context_root: &Path, path: &Path) {
    let Some((project, name)) = relative_file(context_root, path) else {
        return;
    };
    if validate_relative_path(&name).is_err() {
        return;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return, // gone already, a directory, or a symlink: nothing to report
    };
    if metadata.len() > MAX_PROJECT_BYTES {
        warn!(project = %project, path = %name, "ignoring an oversized shared context file on disk");
        return;
    }
    let Ok(content) = std::fs::read(path) else {
        return; // raced a delete or another write; the next event settles it
    };
    if daemon.context.matches_recorded(project, &name, &content) {
        return; // no new information: wispd's own write, or the same content seen again
    }
    daemon
        .context
        .record_external_write(project, &name, &content);
    let writer = daemon.context.writer_of(project, &name);
    let file = context_file(&name, &metadata, writer);
    let seq = daemon.log.append(
        jiff::Timestamp::now(),
        Some(project),
        wisp_protocol::WispEvent::ContextChanged { file },
    );
    tracing::info!(project = %project, path = %name, seq, "a shared context file changed on disk");
}
