use std::collections::BTreeMap;

use futures_util::future::BoxFuture;

use crate::{
    connection::ConnectionTier,
    database::{
        query::QueryOutput,
        table::{ApplyError, EditSnapshot, StagedEdits, TablePreview},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColumnMetadata {
    pub(crate) name: String,
    pub(crate) data_type: String,
    pub(crate) udt_name: String,
    pub(crate) nullable: bool,
    pub(crate) has_default: bool,
    pub(crate) identity: Option<String>,
    pub(crate) generated: Option<String>,
    pub(crate) primary_key: bool,
    pub(crate) foreign_key: bool,
    pub(crate) description: Option<String>,
    pub(crate) postgis_type: Option<String>,
    pub(crate) postgis_srid: Option<i32>,
    pub(crate) postgis_dimensions: Option<i32>,
}

impl ColumnMetadata {
    pub(crate) fn detail(&self) -> String {
        let mut detail = vec![self.data_type.clone()];
        if !self.nullable {
            detail.push("not null".into());
        }
        if self.primary_key {
            detail.push("primary key".into());
        } else if self.foreign_key {
            detail.push("foreign key".into());
        }
        if let Some(identity) = &self.identity {
            detail.push(format!("identity {identity}"));
        } else if let Some(generated) = &self.generated {
            detail.push(format!("generated {generated}"));
        } else if self.has_default {
            detail.push("has default".into());
        }
        if let Some(postgis_type) = &self.postgis_type {
            let mut spatial = postgis_type.clone();
            if let Some(srid) = self.postgis_srid {
                spatial.push_str(&format!(", SRID {srid}"));
            }
            if let Some(dimensions) = self.postgis_dimensions {
                spatial.push_str(&format!(", {dimensions}D"));
            }
            detail.push(spatial);
        }
        detail.join(" · ")
    }

    pub(crate) fn is_spatial(&self) -> bool {
        matches!(self.udt_name.as_str(), "geometry" | "geography") || self.postgis_type.is_some()
    }
}

/// The browsing information for one connection and selected namespace.
/// Engines without namespaces can return an empty schema and schema list.
pub(crate) struct DatabaseMetadata {
    pub(crate) schema: String,
    pub(crate) databases: Vec<String>,
    pub(crate) schemas: Vec<String>,
    pub(crate) tables: Vec<String>,
    pub(crate) columns: BTreeMap<String, Vec<ColumnMetadata>>,
}

/// A configured backend capable of opening a connection to a database.
/// Implementations must not expose credentials in user-facing errors.
pub(crate) trait DatabaseDriver: Send + Sync {
    /// A stable, engine-specific identifier, unique across driver implementations.
    fn engine(&self) -> &'static str;
    fn id(&self) -> &str;
    fn tier(&self) -> ConnectionTier;
    fn connect<'a>(
        &'a self,
        database: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseConnection>, String>>;
}

/// Operations shared by the currently supported browsing and SQL workflows.
/// Unsupported operations must return an error rather than silently succeeding.
pub(crate) trait DatabaseConnection: Send + Sync {
    fn metadata(
        &self,
        preferred_schema: Option<String>,
        require_schema: bool,
    ) -> BoxFuture<'_, Result<DatabaseMetadata, String>>;
    fn ping(&self) -> BoxFuture<'_, Result<(), String>>;
    fn execute_sql<'a>(
        &'a self,
        schema: String,
        sql: String,
    ) -> BoxFuture<'a, Result<QueryOutput, String>>;

    /// Engines that do not support previews must return an explicit error.
    /// Editable values must correspond to the returned preview's row and column order.
    /// An engine may return no edit snapshot for readonly or unsupported tables.
    fn preview_table<'a>(
        &'a self,
        _schema: &'a str,
        _table: &'a str,
        _allow_editing: bool,
    ) -> BoxFuture<'a, Result<(TablePreview, Option<EditSnapshot>), String>> {
        Box::pin(async { Err("Table previews are not supported for this connection yet.".into()) })
    }

    /// Return `CommitUnknown` whenever a commit may have occurred; return
    /// `Rejected` only when no commit occurred.
    fn apply_table_edits<'a>(
        &'a self,
        _schema: &'a str,
        _table: &'a str,
        _snapshot: &'a EditSnapshot,
        _edits: &'a StagedEdits,
    ) -> BoxFuture<'a, Result<(), ApplyError>> {
        Box::pin(async {
            Err(ApplyError::Rejected(
                "Table editing is not supported for this connection yet.".into(),
            ))
        })
    }
}
