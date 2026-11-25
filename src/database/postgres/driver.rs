use std::collections::BTreeMap;

use futures_util::future::BoxFuture;
use sqlx::PgPool;

use crate::{
    connection::{
        ConnectionTier,
        postgres::{ConnectionConfig, connect_direct},
    },
    database::{
        driver::{ColumnMetadata, DatabaseConnection, DatabaseDriver, DatabaseMetadata},
        query::QueryOutput,
        table::{ApplyError, EditSnapshot, StagedEdits, TablePreview},
    },
};

#[cfg(test)]
use crate::connection::manager::ManagedConnection;

use super::{query_execution, table_edits, table_preview};

impl DatabaseDriver for ConnectionConfig {
    fn engine(&self) -> &'static str {
        "postgresql"
    }

    fn id(&self) -> &str {
        self.id()
    }

    fn tier(&self) -> ConnectionTier {
        self.tier()
    }

    fn connect<'a>(
        &'a self,
        database: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseConnection>, String>> {
        Box::pin(async move {
            let pool = connect_direct(self, database).await?;
            Ok(Box::new(PostgresConnection { pool }) as Box<dyn DatabaseConnection>)
        })
    }
}

struct PostgresConnection {
    pool: PgPool,
}

impl DatabaseConnection for PostgresConnection {
    fn metadata(
        &self,
        preferred_schema: Option<String>,
        require_schema: bool,
    ) -> BoxFuture<'_, Result<DatabaseMetadata, String>> {
        Box::pin(async move {
            let databases = sqlx::query_scalar::<_, String>(
                "SELECT datname::text FROM pg_catalog.pg_database WHERE datallowconn AND NOT datistemplate AND has_database_privilege(oid, 'CONNECT') ORDER BY datname",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|_| "Failed to load databases".to_owned())?;
            let schemas = sqlx::query_scalar::<_, String>(
                "SELECT nspname::text FROM pg_catalog.pg_namespace WHERE nspname <> 'information_schema' AND nspname NOT LIKE 'pg\\_%' ESCAPE '\\' AND has_schema_privilege(oid, 'USAGE') ORDER BY nspname",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|_| "Failed to load schemas".to_owned())?;

            let schema =
                if let Some(schema) = preferred_schema.filter(|schema| schemas.contains(schema)) {
                    schema
                } else if require_schema {
                    return Err("Schema no longer exists or is not accessible".to_owned());
                } else {
                    let current =
                        sqlx::query_scalar::<_, Option<String>>("SELECT current_schema()::text")
                            .fetch_one(&self.pool)
                            .await
                            .map_err(|_| "Failed to determine the current schema".to_owned())?;
                    current
                        .filter(|schema| schemas.contains(schema))
                        .or_else(|| schemas.first().cloned())
                        .unwrap_or_default()
                };

            let tables = if schema.is_empty() {
                Vec::new()
            } else {
                sqlx::query_scalar::<_, String>(
                    "SELECT c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'f') AND pg_catalog.has_schema_privilege(n.oid, 'USAGE') AND (pg_catalog.has_table_privilege(c.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER') OR pg_catalog.has_any_column_privilege(c.oid, 'SELECT, INSERT, UPDATE, REFERENCES')) ORDER BY c.relname",
                )
                .bind(&schema)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "Failed to load tables".to_owned())?
            };

            let columns = if schema.is_empty() {
                BTreeMap::new()
            } else {
                let rows = sqlx::query_as::<_, (
                    String,
                    String,
                    String,
                    String,
                    bool,
                    bool,
                    Option<String>,
                    Option<String>,
                    bool,
                    bool,
                    Option<String>,
                )>(
                    "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod)::text, t.typname::text, NOT a.attnotnull, a.atthasdef, CASE a.attidentity WHEN 'a' THEN 'always' WHEN 'd' THEN 'by default' ELSE NULL END, CASE a.attgenerated WHEN 's' THEN 'stored' ELSE NULL END, COALESCE(pk.attnum IS NOT NULL, false), COALESCE(fk.attnum IS NOT NULL, false), col_description(c.oid, a.attnum) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid JOIN pg_catalog.pg_type t ON t.oid = a.atttypid LEFT JOIN (SELECT conrelid, unnest(conkey) AS attnum FROM pg_catalog.pg_constraint WHERE contype = 'p') pk ON pk.conrelid = c.oid AND pk.attnum = a.attnum LEFT JOIN (SELECT conrelid, unnest(conkey) AS attnum FROM pg_catalog.pg_constraint WHERE contype = 'f') fk ON fk.conrelid = c.oid AND fk.attnum = a.attnum WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'f') AND a.attnum > 0 AND NOT a.attisdropped AND pg_catalog.has_column_privilege(c.oid, a.attnum, 'SELECT') ORDER BY c.relname, a.attnum",
                )
                .bind(&schema)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "Failed to load columns".to_owned())?;
                let mut columns: BTreeMap<String, Vec<ColumnMetadata>> = BTreeMap::new();
                for (
                    table,
                    name,
                    data_type,
                    udt_name,
                    nullable,
                    has_default,
                    identity,
                    generated,
                    primary_key,
                    foreign_key,
                    description,
                ) in rows
                {
                    columns.entry(table).or_default().push(ColumnMetadata {
                        name,
                        data_type,
                        udt_name,
                        nullable,
                        has_default,
                        identity,
                        generated,
                        primary_key,
                        foreign_key,
                        description,
                        postgis_type: None,
                        postgis_srid: None,
                        postgis_dimensions: None,
                    });
                }
                let spatial_rows = sqlx::query_as::<_, (String, String, String, i32, i32)>(
                    "SELECT f_table_name::text, f_geometry_column::text, type::text, srid::integer, coord_dimension::integer FROM public.geometry_columns WHERE f_table_schema = $1 UNION ALL SELECT f_table_name::text, f_geography_column::text, type::text, srid::integer, coord_dimension::integer FROM public.geography_columns WHERE f_table_schema = $1",
                )
                .bind(&schema)
                .fetch_all(&self.pool)
                .await;
                if let Ok(spatial_rows) = spatial_rows {
                    for (table, column_name, kind, srid, dimensions) in spatial_rows {
                        if let Some(metadata) = columns.get_mut(&table).and_then(|columns| {
                            columns
                                .iter_mut()
                                .find(|metadata| metadata.name == column_name)
                        }) {
                            metadata.postgis_type = Some(kind);
                            metadata.postgis_srid = Some(srid);
                            metadata.postgis_dimensions = Some(dimensions);
                        }
                    }
                }
                columns
            };

            Ok(DatabaseMetadata {
                schema,
                databases,
                schemas,
                tables,
                columns,
            })
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .map(|_| ())
                .map_err(|_| "Connection probe failed".to_owned())
        })
    }

    fn execute_sql<'a>(
        &'a self,
        schema: String,
        sql: String,
    ) -> BoxFuture<'a, Result<QueryOutput, String>> {
        Box::pin(query_execution::execute(self.pool.clone(), schema, sql))
    }

    fn preview_table<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
        allow_editing: bool,
    ) -> BoxFuture<'a, Result<(TablePreview, Option<EditSnapshot>), String>> {
        Box::pin(async move {
            let (preview, snapshot) =
                table_preview::load_editable_preview(&self.pool, schema, table, allow_editing)
                    .await?;
            Ok((
                preview,
                snapshot.map(table_edits::EditSnapshot::into_shared),
            ))
        })
    }

    fn apply_table_edits<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
        snapshot: &'a EditSnapshot,
        edits: &'a StagedEdits,
    ) -> BoxFuture<'a, Result<(), ApplyError>> {
        Box::pin(async move {
            let snapshot = snapshot
                .token::<table_edits::EditSnapshot>()
                .ok_or_else(|| {
                    ApplyError::Rejected(
                        "The edit snapshot does not match this database driver.".into(),
                    )
                })?;
            table_edits::apply_edits(&self.pool, schema, table, snapshot, edits).await
        })
    }
}

#[cfg(test)]
pub(crate) fn test_connection(
    profile_id: &str,
    database: &str,
    tier: ConnectionTier,
    pool: PgPool,
) -> ManagedConnection {
    ManagedConnection::for_test(
        "postgresql",
        profile_id,
        database,
        tier,
        PostgresConnection { pool },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    #[tokio::test]
    async fn rejects_a_snapshot_from_another_backend_before_touching_the_pool() {
        let pool = PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new());
        pool.close().await;
        let connection = test_connection("local", "db", ConnectionTier::Local, pool);
        let mut snapshot = EditSnapshot::new(Vec::new(), Vec::new(), "another backend");
        snapshot.bind(connection.id(), "public", "example");
        assert!(matches!(
            connection.apply_table_edits("public", "example", &snapshot, &StagedEdits::default()).await,
            Err(ApplyError::Rejected(message)) if message.contains("does not match this database driver")
        ));
    }
}
