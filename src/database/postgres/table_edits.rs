use std::collections::BTreeMap;

use sqlx::{AssertSqlSafe, PgPool, Postgres, QueryBuilder};

use crate::database::table::{
    ApplyError, EditColumn as SharedEditColumn, EditRow as SharedEditRow,
    EditSnapshot as SharedEditSnapshot, StagedEdits,
};

use super::table_preview::{editable_metadata, quote_identifier};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditColumn {
    pub name: String,
    pub editable_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditRow {
    pub table_oid: i64,
    pub ctid: String,
    pub xmin: String,
    pub originals: Vec<Option<String>>,
    pub key_values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditSnapshot {
    pub relation: i64,
    pub columns: Vec<EditColumn>,
    pub keys: Vec<String>,
    pub rows: Vec<EditRow>,
}

impl EditSnapshot {
    pub(super) fn into_shared(self) -> SharedEditSnapshot {
        let columns = self
            .columns
            .iter()
            .map(|column| SharedEditColumn {
                name: column.name.clone(),
                editable: column.editable_type.is_some(),
            })
            .collect();
        let rows = self
            .rows
            .iter()
            .map(|row| SharedEditRow {
                originals: row.originals.clone(),
            })
            .collect();
        SharedEditSnapshot::new(columns, rows, self)
    }
}

pub(crate) async fn apply_edits(
    pool: &PgPool,
    schema: &str,
    table: &str,
    snapshot: &EditSnapshot,
    edits: &StagedEdits,
) -> Result<(), ApplyError> {
    let reject = |message: String| ApplyError::Rejected(message);
    if edits.is_empty() {
        return Err(reject("No changes to apply".into()));
    }
    let qualified = format!(
        "{}.{}",
        quote_identifier(schema).map_err(&reject)?,
        quote_identifier(table).map_err(&reject)?
    );
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| reject(format!("Could not start transaction: {e}")))?;
    sqlx::query("SET LOCAL statement_timeout = '15s'")
        .execute(&mut *tx)
        .await
        .map_err(|e| reject(format!("Could not set timeout: {e}")))?;
    let lock = format!("SELECT 1 FROM {qualified} LIMIT 0");
    sqlx::query(AssertSqlSafe(lock))
        .execute(&mut *tx)
        .await
        .map_err(|e| reject(format!("Could not access table: {e}")))?;
    let relation: Option<i64> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
            .bind(&qualified)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| reject(format!("Could not verify table: {e}")))?;
    if relation != Some(snapshot.relation) {
        return Err(reject(
            "The table changed since the preview was loaded; discard and reload the preview."
                .into(),
        ));
    }
    if editable_metadata(&mut tx, snapshot.relation)
        .await
        .map_err(&reject)?
        != Some((snapshot.columns.clone(), snapshot.keys.clone()))
    {
        return Err(reject(
            "The table schema or primary key changed; discard and reload the preview.".into(),
        ));
    }
    let mut grouped: BTreeMap<usize, Vec<(usize, Option<String>)>> = BTreeMap::new();
    for ((row, col), value) in edits.iter() {
        if snapshot.rows.get(row).is_none()
            || snapshot
                .columns
                .get(col)
                .and_then(|c| c.editable_type.as_ref())
                .is_none()
        {
            return Err(reject("The staged changes are no longer valid.".into()));
        }
        grouped.entry(row).or_default().push((col, value.clone()));
    }
    for (row_ix, changes) in grouped {
        let row = &snapshot.rows[row_ix];
        if row.key_values.len() != snapshot.keys.len() {
            return Err(reject("The row identity is incomplete.".into()));
        }
        let mut query: QueryBuilder<Postgres> =
            QueryBuilder::new(format!("UPDATE {qualified} AS t SET "));
        for (ix, (col_ix, value)) in changes.iter().enumerate() {
            if ix > 0 {
                query.push(", ");
            }
            let column = &snapshot.columns[*col_ix];
            query
                .push(quote_identifier(&column.name).map_err(&reject)?)
                .push(" = ")
                .push_bind(value.clone())
                .push("::text::")
                .push(
                    column
                        .editable_type
                        .as_ref()
                        .expect("validated editable type"),
                );
        }
        query
            .push(" WHERE t.tableoid::bigint = ")
            .push_bind(row.table_oid)
            .push(" AND t.ctid::text = ")
            .push_bind(row.ctid.clone())
            .push(" AND t.xmin::text = ")
            .push_bind(row.xmin.clone());
        for (key, value) in snapshot.keys.iter().zip(&row.key_values) {
            query
                .push(" AND t.")
                .push(quote_identifier(key).map_err(&reject)?)
                .push("::text = ")
                .push_bind(value.clone());
        }
        let result = query
            .build()
            .execute(&mut *tx)
            .await
            .map_err(|e| reject(format!("Could not update preview row {}: {e}", row_ix + 1)))?;
        if result.rows_affected() != 1 {
            return Err(reject(format!(
                "Preview row {} changed or is no longer writable. No changes were applied.",
                row_ix + 1
            )));
        }
    }
    tx.commit().await.map_err(|e| {
        ApplyError::CommitUnknown(format!(
            "The commit result is unknown: {e}. Reload the table before retrying."
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL instance"]
    async fn applies_atomically_and_rejects_conflicts() {
        use crate::{connection::ConnectionTier, database::postgres::test_connection};
        use futures_util::FutureExt as _;
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::panic::AssertUnwindSafe;
        let socket = std::env::var("DBVIEWER_TEST_SOCKET").expect("disposable socket directory");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                PgConnectOptions::new()
                    .socket(socket)
                    .username("postgres")
                    .database("postgres"),
            )
            .await
            .unwrap();
        // The only interpolated identifier is a fixed prefix and hexadecimal UUID.
        let schema = format!("dbviewer_edit_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&pool)
            .await
            .unwrap();
        let result = AssertUnwindSafe(async {
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE {schema}.\"edit test\" (id int PRIMARY KEY, \"str\"\"col\" text, n int, g int GENERATED ALWAYS AS (n + 1) STORED)"))).execute(&pool).await.unwrap();
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {schema}.\"edit test\" (id, \"str\"\"col\", n) VALUES (1, 'one', 1), (2, 'two', 2)"))).execute(&pool).await.unwrap();
            let connection = test_connection(
                "integration",
                "postgres",
                ConnectionTier::Local,
                pool.clone(),
            );
            let (_, snapshot) = connection
                .preview_table(&schema, "edit test")
                .await
                .unwrap();
            let snapshot = snapshot.unwrap();
            let native = snapshot.token::<EditSnapshot>().unwrap();
            assert_eq!(native.keys, ["id"]);
            assert!(!snapshot.columns[0].editable);
            assert!(!snapshot.columns[3].editable);
            let first = native
                .rows
                .iter()
                .position(|row| row.key_values == ["1"])
                .unwrap();
            let second = native
                .rows
                .iter()
                .position(|row| row.key_values == ["2"])
                .unwrap();
            let mut edits = StagedEdits::default();
            assert!(edits.stage(&snapshot, first, 1, Some("quote ' and newline\n".into())));
            assert!(edits.stage(&snapshot, second, 2, Some("bad integer".into())));
            assert!(matches!(
                connection
                    .apply_table_edits(&schema, "edit test", &snapshot, &edits)
                    .await,
                Err(ApplyError::Rejected(_))
            ));
            let value: String =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT \"str\"\"col\" FROM {schema}.\"edit test\" WHERE id = 1")))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(value, "one");
            assert!(edits.stage(&snapshot, second, 2, Some("42".into())));
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {schema}.\"edit test\" SET n = 3 WHERE id = 2")))
                .execute(&pool)
                .await
                .unwrap();
            assert!(matches!(
                connection
                    .apply_table_edits(&schema, "edit test", &snapshot, &edits)
                    .await,
                Err(ApplyError::Rejected(_))
            ));
            let value: String =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT \"str\"\"col\" FROM {schema}.\"edit test\" WHERE id = 1")))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(value, "one");
            let (_, current) = connection
                .preview_table(&schema, "edit test")
                .await
                .unwrap();
            let current = current.unwrap();
            let native = current.token::<EditSnapshot>().unwrap();
            let first = native
                .rows
                .iter()
                .position(|row| row.key_values == ["1"])
                .unwrap();
            let second = native
                .rows
                .iter()
                .position(|row| row.key_values == ["2"])
                .unwrap();
            let mut edits = StagedEdits::default();
            assert!(edits.stage(&current, first, 1, None));
            assert!(edits.stage(&current, second, 2, Some("42".into())));
            connection
                .apply_table_edits(&schema, "edit test", &current, &edits)
                .await
                .unwrap();
            let value: Option<String> =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT \"str\"\"col\" FROM {schema}.\"edit test\" WHERE id = 1")))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let number: i32 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT n FROM {schema}.\"edit test\" WHERE id = 2")))
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(value, None);
            assert_eq!(number, 42);
            assert!(matches!(
                connection
                    .apply_table_edits(&schema, "edit test", &current, &edits)
                    .await,
                Err(ApplyError::Rejected(_))
            ));
        }).catch_unwind().await;
        let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&pool)
            .await;
        pool.close().await;
        if let Err(panic) = result {
            if let Err(error) = cleanup {
                eprintln!("Could not remove disposable edit fixture {schema}: {error}");
            }
            std::panic::resume_unwind(panic);
        }
        cleanup.expect("remove disposable edit fixture");
    }

    #[test]
    fn staging_tracks_originals_and_distinguishes_null() {
        let snapshot = EditSnapshot {
            relation: 1,
            columns: vec![EditColumn {
                name: "value".into(),
                editable_type: Some("\"pg_catalog\".\"text\"".into()),
            }],
            keys: vec!["id".into()],
            rows: vec![EditRow {
                table_oid: 1,
                ctid: "(0,1)".into(),
                xmin: "2".into(),
                originals: vec![Some("before".into())],
                key_values: vec!["1".into()],
            }],
        };
        let snapshot = snapshot.into_shared();
        let mut changes = StagedEdits::default();
        assert!(changes.stage(&snapshot, 0, 0, None));
        assert_eq!(changes.get(0, 0), Some(&None));
        assert_eq!(changes.len(), 1);
        assert!(changes.stage(&snapshot, 0, 0, Some("before".into())));
        assert!(changes.is_empty());
        assert!(!changes.stage(&snapshot, 1, 0, Some("x".into())));
    }

    #[test]
    fn invalid_or_readonly_cells_do_not_disturb_staged_changes() {
        let snapshot = EditSnapshot {
            relation: 1,
            columns: vec![
                EditColumn {
                    name: "value".into(),
                    editable_type: Some("\"pg_catalog\".\"text\"".into()),
                },
                EditColumn {
                    name: "id".into(),
                    editable_type: None,
                },
            ],
            keys: vec!["id".into()],
            rows: vec![EditRow {
                table_oid: 1,
                ctid: "(0,1)".into(),
                xmin: "2".into(),
                originals: vec![None, Some("1".into())],
                key_values: vec!["1".into()],
            }],
        };
        let snapshot = snapshot.into_shared();
        let mut changes = StagedEdits::default();
        assert!(changes.stage(&snapshot, 0, 0, Some(String::new())));
        assert!(!changes.stage(&snapshot, 0, 1, Some("2".into())));
        assert!(!changes.stage(&snapshot, 0, 2, Some("invalid".into())));
        assert!(!changes.stage(&snapshot, 1, 0, Some("invalid".into())));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes.get(0, 0), Some(&Some(String::new())));
        assert!(changes.stage(&snapshot, 0, 0, None));
        assert!(changes.is_empty());
    }

    #[tokio::test]
    async fn applying_an_empty_change_set_is_rejected_without_a_connection() {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let pool = PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new());
        pool.close().await;
        let snapshot = EditSnapshot {
            relation: 1,
            columns: Vec::new(),
            keys: Vec::new(),
            rows: Vec::new(),
        };
        assert!(matches!(
            apply_edits(&pool, "public", "example", &snapshot, &StagedEdits::default()).await,
            Err(ApplyError::Rejected(message)) if message == "No changes to apply"
        ));
    }
}
