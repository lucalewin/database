use futures_util::TryStreamExt;
use sqlx::{AssertSqlSafe, Column as _, Either, PgPool, Row as _, TypeInfo as _, ValueRef as _};

use super::postgis;

use crate::database::{
    query::{QueryOutput, StatementOutput},
    table::TablePreview,
};

const MAX_ROWS: usize = 500;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_VALUE_BYTES: usize = 2048;
const MAX_STATEMENTS: usize = 500;
const MAX_COLUMNS: usize = 512;

// Account for collection entries as well as text so empty values cannot bypass the budget.
struct Output {
    result: QueryOutput,
    bytes: usize,
    rows: usize,
}

impl Output {
    fn new() -> Self {
        Self {
            result: QueryOutput::default(),
            bytes: 0,
            rows: 0,
        }
    }

    fn reserve(&mut self, size: usize) -> bool {
        if size > MAX_OUTPUT_BYTES.saturating_sub(self.bytes) {
            self.result.truncated = true;
            false
        } else {
            self.bytes += size;
            true
        }
    }

    fn value(&mut self, value: &str) -> String {
        let end = floor_char_boundary(value, value.len().min(MAX_VALUE_BYTES));
        let suffix = if end < value.len() {
            self.result.truncated = true;
            "…"
        } else {
            ""
        };
        format!("{}{suffix}", &value[..end])
    }

    fn statement(&mut self) -> Option<usize> {
        if self.result.statements.len() >= MAX_STATEMENTS || !self.reserve(32) {
            self.result.truncated = true;
            return None;
        }
        let ix = self.result.statements.len();
        self.result.statements.push(StatementOutput::default());
        Some(ix)
    }

    fn finish(self) -> QueryOutput {
        self.result
    }
}

fn floor_char_boundary(value: &str, mut end: usize) -> usize {
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Execute on a disposable session so SQL session state never contaminates the browser pool.
pub(crate) async fn execute(
    pool: PgPool,
    schema: String,
    sql: String,
) -> Result<QueryOutput, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Could not acquire a connection: {error}"))?;
    connection.close_on_drop();

    if !schema.is_empty() {
        sqlx::query(
            "SELECT pg_catalog.set_config('search_path', pg_catalog.quote_ident($1), false)",
        )
        .bind(&schema)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Could not select schema: {error}"))?;
    }
    sqlx::query("SELECT pg_catalog.set_config('statement_timeout', '30s', false)")
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Could not set statement timeout: {error}"))?;

    let mut output = Output::new();
    let mut current = None;
    let mut has_rows = false;
    let mut spatial_columns = Vec::new();
    let mut stream = sqlx::raw_sql(AssertSqlSafe(sql)).fetch_many(&mut *connection);
    loop {
        match stream.try_next().await {
            Ok(Some(Either::Right(row))) => {
                if !has_rows {
                    current = output.statement();
                    has_rows = true;
                    if let Some(ix) = current {
                        if row.len() > MAX_COLUMNS {
                            output.result.truncated = true;
                        }
                        let mut columns = Vec::new();
                        spatial_columns.clear();
                        for column in row.columns().iter().take(MAX_COLUMNS) {
                            spatial_columns
                                .push(postgis::is_spatial_type(column.type_info().name()));
                            let name = output.value(column.name());
                            if !output.reserve(24 + name.len()) {
                                break;
                            }
                            columns.push(name);
                        }
                        if !columns.is_empty() {
                            output.result.statements[ix].table = Some(TablePreview {
                                columns,
                                rows: Vec::new(),
                            });
                        }
                    }
                }
                let Some(ix) = current else { continue };
                let Some(table) = output.result.statements[ix].table.as_ref() else {
                    continue;
                };
                let count = table.columns.len();
                if output.rows >= MAX_ROWS || !output.reserve(24 + count * 24) {
                    output.result.truncated = true;
                    continue;
                }
                let mut values = Vec::with_capacity(count);
                let mut incomplete = false;
                for col in 0..count {
                    let raw = match row.try_get_raw(col) {
                        Ok(raw) => raw,
                        Err(error) => {
                            output.result.error = Some(format!("Could not read result: {error}"));
                            break;
                        }
                    };
                    let value = if raw.is_null() {
                        None
                    } else {
                        match raw.as_str() {
                            Ok(value) => {
                                let displayed = spatial_columns
                                    .get(col)
                                    .copied()
                                    .unwrap_or(false)
                                    .then(|| postgis::display_value(value));
                                Some(output.value(displayed.as_deref().unwrap_or(value)))
                            }
                            Err(error) => {
                                output.result.error =
                                    Some(format!("Could not decode result: {error}"));
                                break;
                            }
                        }
                    };
                    if !output.reserve(value.as_ref().map_or(0, String::len)) {
                        incomplete = true;
                        break;
                    }
                    values.push(value);
                }
                if output.result.error.is_some() {
                    break;
                }
                if !incomplete {
                    output.result.statements[ix]
                        .table
                        .as_mut()
                        .unwrap()
                        .rows
                        .push(values);
                    output.rows += 1;
                }
            }
            Ok(Some(Either::Left(result))) => {
                let ix = if has_rows {
                    current
                } else {
                    output.statement()
                };
                if let Some(ix) = ix {
                    output.result.statements[ix].rows_affected = Some(result.rows_affected());
                }
                has_rows = false;
                current = None;
            }
            Ok(None) => break,
            Err(error) => {
                output.result.error = Some(format!("Query failed: {error}"));
                break;
            }
        }
    }
    Ok(output.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_bounded_and_respects_utf8() {
        let mut output = Output::new();
        let value = output.value(&"é".repeat(MAX_VALUE_BYTES));
        assert!(output.result.truncated);
        assert!(value.contains('…'));
        assert!(output.reserve(MAX_OUTPUT_BYTES));
        assert!(!output.reserve(1));
        assert!(output.finish().truncated);
    }

    #[test]
    fn output_preserves_null_empty_and_control_characters() {
        let mut output = Output::new();
        let row = vec![
            None,
            Some(output.value("")),
            Some(output.value("NULL\tline\n")),
        ];
        assert_eq!(
            row,
            vec![None, Some(String::new()), Some("NULL\tline\n".into())]
        );
    }

    #[tokio::test]
    #[ignore = "requires the local Compose PostgreSQL database"]
    async fn runs_queries_and_keeps_session_state_out_of_the_pool() {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("dbviewer")
            .password("dbviewer")
            .database("dbviewer")
            .ssl_mode(PgSslMode::Disable);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let output = execute(
            pool.clone(),
            "public".into(),
            "SELECT NULL AS n, 'NULL' AS s, 42 AS value; SELECT count(*) AS total FROM customers"
                .into(),
        )
        .await
        .unwrap();
        let row = &output.statements[0].table.as_ref().unwrap().rows[0];
        assert_eq!(row, &[None, Some("NULL".into()), Some("42".into())]);
        assert_eq!(output.statements.len(), 2);
        let error = execute(
            pool.clone(),
            "public".into(),
            "SELECT missing_column".into(),
        )
        .await
        .unwrap();
        assert!(error.error.unwrap().contains("Query failed"));
        let scoped = execute(
            pool.clone(),
            "inventory".into(),
            "SELECT count(*) FROM products".into(),
        )
        .await
        .unwrap();
        assert!(scoped.error.is_none(), "{scoped:?}");
        let default_path: String = sqlx::query_scalar("SHOW search_path")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(default_path, "inventory");
    }
}
