use jiff::Timestamp;
use rusqlite::{Row, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// One item of a Project's inbox (PLX-401, decision 0043). The daemon owns what `kind` means;
/// this crate stores it as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The run it is about: a child, or the coordinator for paused wake-ups.
    pub run_id: Uuid,
    pub kind: String,
    pub text: String,
    pub created_at: Timestamp,
    pub seen_at: Option<Timestamp>,
}

const COLUMNS: &str = "id, project_id, run_id, kind, text, created_at, seen_at";

/// A row as SQLite stored it, before the fallible conversion to [`InboxItem`].
type Raw = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

fn from_row(row: &Row<'_>) -> rusqlite::Result<Raw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn into_item((id, project, run, kind, text, created, seen): Raw) -> Result<InboxItem, StoreError> {
    Ok(InboxItem {
        id: Uuid::parse_str(&id)?,
        project_id: Uuid::parse_str(&project)?,
        run_id: Uuid::parse_str(&run)?,
        kind,
        text,
        created_at: timestamp::parse(&created)?,
        seen_at: seen.as_deref().map(timestamp::parse).transpose()?,
    })
}

impl Store {
    /// Adds `item` to its project's inbox.
    ///
    /// # Errors
    ///
    /// A database error, including a constraint error if its id is taken.
    pub fn add_inbox_item(&self, item: &InboxItem) -> Result<(), StoreError> {
        self.conn.execute(
            &format!("INSERT INTO inbox ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
            params![
                item.id.to_string(),
                item.project_id.to_string(),
                item.run_id.to_string(),
                item.kind,
                item.text,
                timestamp::format(item.created_at),
                item.seen_at.map(timestamp::format),
            ],
        )?;
        Ok(())
    }

    /// `project`'s inbox, oldest first.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or time is corrupt.
    pub fn inbox(&self, project: Uuid) -> Result<Vec<InboxItem>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM inbox WHERE project_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = statement
            .query_map(params![project.to_string()], from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(into_item).collect()
    }

    /// Marks `items` of `project`'s inbox seen at `at`, keeping the time of any already seen, and
    /// returns them as they stand, oldest first. Ids not in `project`'s inbox are skipped.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn mark_inbox_seen(
        &mut self,
        project: Uuid,
        items: &[Uuid],
        at: Timestamp,
    ) -> Result<Vec<InboxItem>, StoreError> {
        let tx = self.conn.transaction()?;
        for id in items {
            tx.execute(
                "UPDATE inbox SET seen_at = ?3
                 WHERE id = ?1 AND project_id = ?2 AND seen_at IS NULL",
                params![id.to_string(), project.to_string(), timestamp::format(at)],
            )?;
        }
        tx.commit()?;
        Ok(self
            .inbox(project)?
            .into_iter()
            .filter(|item| items.contains(&item.id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use uuid::Uuid;

    use super::InboxItem;
    use crate::Store;

    fn item(project: Uuid, text: &str) -> InboxItem {
        InboxItem {
            id: Uuid::now_v7(),
            project_id: project,
            run_id: Uuid::now_v7(),
            kind: "done".to_owned(),
            text: text.to_owned(),
            created_at: Timestamp::now(),
            seen_at: None,
        }
    }

    #[test]
    fn items_list_per_project_and_keep_their_first_seen_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("parallax.sqlite3")).unwrap();
        let (ours, theirs) = (Uuid::now_v7(), Uuid::now_v7());
        let (first, second, other) = (item(ours, "a"), item(ours, "b"), item(theirs, "c"));
        for item in [&first, &second, &other] {
            store.add_inbox_item(item).unwrap();
        }
        assert_eq!(store.inbox(ours).unwrap(), [first.clone(), second.clone()]);

        let at: Timestamp = "2026-10-03T12:00:00Z".parse().unwrap();
        let seen = store
            .mark_inbox_seen(ours, &[first.id, other.id], at)
            .unwrap();
        assert_eq!(seen.len(), 1, "another project's item is skipped");
        assert_eq!(seen[0].seen_at, Some(at));
        let later: Timestamp = "2026-10-03T13:00:00Z".parse().unwrap();
        let again = store.mark_inbox_seen(ours, &[first.id], later).unwrap();
        assert_eq!(again[0].seen_at, Some(at), "the first time is kept");
        assert_eq!(store.inbox(theirs).unwrap()[0].seen_at, None);

        store.delete_project(ours).unwrap();
        assert!(store.inbox(ours).unwrap().is_empty());
    }
}
