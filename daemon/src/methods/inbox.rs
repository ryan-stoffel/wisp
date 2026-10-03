//! A Project's inbox (PLX-401, decision 0043): `inbox/list` and `inbox/seen`, behind the `inbox`
//! capability, and [`add`], which every source of an item calls.

use std::sync::Arc;

use jiff::Timestamp;
use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    ErrorKind, InboxItem, InboxItemId, InboxKind, InboxListParams, InboxListResult,
    InboxSeenParams, InboxSeenResult, ParallaxEvent, ProjectId, RunId,
};
use parallax_store::Store;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::Context;
use crate::server::Daemon;
use crate::store::store_error;

/// `project`'s inbox, oldest first, with the `seq` of the last event it reflects.
pub(crate) async fn list(
    context: &Context,
    params: InboxListParams,
) -> Result<InboxListResult, ErrorObject> {
    let log = Arc::clone(&context.daemon.log);
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            found(store, params.project)?;
            let items = store
                .inbox(params.project.into())
                .map_err(|error| store_error(&error))?
                .into_iter()
                .map(item)
                .collect::<Result<_, _>>()?;
            Ok(InboxListResult {
                items,
                seq: log.head(),
            })
        })
        .await
}

/// Marks items of `project`'s inbox seen now, and returns them as they stand.
pub(crate) async fn seen(
    context: &Context,
    params: InboxSeenParams,
) -> Result<InboxSeenResult, ErrorObject> {
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            found(store, params.project)?;
            let ids: Vec<_> = params.items.into_iter().map(Into::into).collect();
            let rows = store
                .mark_inbox_seen(params.project.into(), &ids, Timestamp::now())
                .map_err(|error| store_error(&error))?;
            let items = rows.into_iter().map(item).collect::<Result<_, _>>()?;
            Ok(InboxSeenResult { items })
        })
        .await
}

/// Adds an item about `run` to `project`'s inbox and appends `inbox.added`, in one store job so
/// `inbox/list`'s `seq` always agrees with its items. Never fails its caller: a store error is
/// logged.
pub(crate) async fn add(
    daemon: &Arc<Daemon>,
    project: ProjectId,
    run: RunId,
    kind: InboxKind,
    text: String,
) {
    let log = Arc::clone(&daemon.log);
    let added = daemon
        .store
        .run(&CancellationToken::new(), move |store| {
            let row = parallax_store::InboxItem {
                id: InboxItemId::generate().into(),
                project_id: project.into(),
                run_id: run.into(),
                kind: kind_name(kind),
                text,
                created_at: Timestamp::now(),
                seen_at: None,
            };
            store
                .add_inbox_item(&row)
                .map_err(|error| store_error(&error))?;
            let item = item(row)?;
            let seq = log.append_blocking(
                item.created_at,
                Some(project),
                ParallaxEvent::InboxAdded { item },
            );
            info!(%project, %run, seq, "added an inbox item");
            Ok(())
        })
        .await;
    if let Err(error) = added {
        warn!(%project, %run, error = %error.message, "could not add an inbox item");
    }
}

/// `projectNotFound` unless `project` exists.
fn found(store: &Store, project: ProjectId) -> Result<(), ErrorObject> {
    match store.get_project(project.into()) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ErrorObject::parallax(
            ErrorKind::ProjectNotFound,
            format!("no project has id {project}"),
        )),
        Err(error) => Err(store_error(&error)),
    }
}

fn item(row: parallax_store::InboxItem) -> Result<InboxItem, ErrorObject> {
    let invalid =
        |_| ErrorObject::internal_error(format!("inbox item {} has an invalid id", row.id));
    Ok(InboxItem {
        id: InboxItemId::try_from(row.id).map_err(invalid)?,
        run: RunId::try_from(row.run_id).map_err(invalid)?,
        kind: serde_json::from_value(row.kind.into()).unwrap_or(InboxKind::Unknown),
        text: row.text,
        created_at: row.created_at,
        seen_at: row.seen_at,
    })
}

/// `kind`'s name on the wire, which the store keeps.
fn kind_name(kind: InboxKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
