use std::{any::Any, collections::BTreeMap, sync::Arc};

use uuid::Uuid;

/// A table snapshot in column order. `None` represents SQL NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TablePreview {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Option<String>>>,
}

#[derive(Clone)]
pub(crate) struct EditColumn {
    pub(crate) name: String,
    pub(crate) editable: bool,
}

#[derive(Clone)]
pub(crate) struct EditRow {
    pub(crate) originals: Vec<Option<String>>,
}

/// The values needed by the editor and an opaque engine-specific conflict token.
#[derive(Clone)]
pub(crate) struct EditSnapshot {
    pub(crate) columns: Arc<[EditColumn]>,
    pub(crate) rows: Arc<[EditRow]>,
    token: Arc<dyn Any + Send + Sync>,
    scope: Option<EditScope>,
}

#[derive(Clone)]
struct EditScope {
    connection_id: Uuid,
    schema: String,
    table: String,
}

impl EditSnapshot {
    pub(crate) fn new(
        columns: Vec<EditColumn>,
        rows: Vec<EditRow>,
        token: impl Any + Send + Sync,
    ) -> Self {
        Self {
            columns: columns.into(),
            rows: rows.into(),
            token: Arc::new(token),
            scope: None,
        }
    }

    pub(crate) fn token<T: Any>(&self) -> Option<&T> {
        self.token.downcast_ref()
    }

    pub(crate) fn bind(&mut self, connection_id: Uuid, schema: &str, table: &str) {
        self.scope = Some(EditScope {
            connection_id,
            schema: schema.to_owned(),
            table: table.to_owned(),
        });
    }

    pub(crate) fn matches_scope(&self, connection_id: Uuid, schema: &str, table: &str) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            scope.connection_id == connection_id && scope.schema == schema && scope.table == table
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StagedEdits {
    values: BTreeMap<(usize, usize), Option<String>>,
}

impl StagedEdits {
    pub(crate) fn stage(
        &mut self,
        snapshot: &EditSnapshot,
        row: usize,
        col: usize,
        value: Option<String>,
    ) -> bool {
        let Some(column) = snapshot.columns.get(col) else {
            return false;
        };
        let Some(original) = snapshot
            .rows
            .get(row)
            .and_then(|row| row.originals.get(col))
        else {
            return false;
        };
        if !column.editable {
            return false;
        }
        if *original == value {
            self.values.remove(&(row, col));
        } else {
            self.values.insert((row, col), value);
        }
        true
    }

    pub(crate) fn get(&self, row: usize, col: usize) -> Option<&Option<String>> {
        self.values.get(&(row, col))
    }
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub(crate) fn clear(&mut self) {
        self.values.clear();
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = ((usize, usize), &Option<String>)> {
        self.values.iter().map(|(key, value)| (*key, value))
    }
}

#[derive(Debug)]
pub(crate) enum ApplyError {
    /// No commit occurred; staged edits may remain available for correction.
    Rejected(String),
    /// A commit may have occurred; the preview must be verified before retrying.
    CommitUnknown(String),
}
