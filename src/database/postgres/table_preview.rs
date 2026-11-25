use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction};

use crate::database::table::TablePreview;

use super::table_edits::{EditColumn, EditRow, EditSnapshot};

pub(super) const MAX_PREVIEW_ROWS: i64 = 500;

// Non-null values use PostgreSQL's JSON conversion, which may not preserve
// the original textual form. JSON null remains distinct from SQL NULL.
pub(crate) async fn load_editable_preview(
    pool: &PgPool,
    schema: &str,
    table: &str,
    allow_editing: bool,
) -> Result<(TablePreview, Option<EditSnapshot>), String> {
    load_preview(pool, schema, table, allow_editing).await
}

async fn load_preview(
    pool: &PgPool,
    schema: &str,
    table: &str,
    allow_editing: bool,
) -> Result<(TablePreview, Option<EditSnapshot>), String> {
    let qualified = format!("{}.{}", quote_identifier(schema)?, quote_identifier(table)?);
    let context = format!("{schema:?}.{table:?}");
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| format!("Failed to start preview for {context}: {error}"))?;

    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to start readonly preview for {context}: {error}"))?;
    sqlx::query("SET LOCAL statement_timeout = '15s'")
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to set preview timeout for {context}: {error}"))?;

    // Look up the exact names before interpolating them: PostgreSQL truncates
    // overlong SQL identifiers, which must not select a similarly named table.
    let relation: Option<i64> = sqlx::query_scalar(
        "SELECT c.oid::bigint FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname::text = $1 AND c.relname::text = $2 \
         AND c.relkind IN ('r', 'p', 'f')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to find table {context}: {error}"))?;
    let relation = relation.ok_or_else(|| format!("Table {context} no longer exists"))?;

    // A SELECT takes the same access-share lock without requiring table-level
    // privileges or rejecting foreign tables, unlike an explicit LOCK TABLE.
    let lock_sql = format!("SELECT 1 FROM {qualified} LIMIT 0");
    sqlx::query(AssertSqlSafe(lock_sql))
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to access table {context} for preview: {error}"))?;

    let locked_relation: Option<i64> = sqlx::query_scalar(
        "SELECT c.oid::bigint FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname::text = $1 AND c.relname::text = $2 \
         AND c.relkind IN ('r', 'p', 'f')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to verify table {context}: {error}"))?;
    if locked_relation != Some(relation) {
        return Err(format!("Table {context} changed while loading its preview"));
    }

    let column_metadata: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.attname::text, t.typname::text FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
         WHERE a.attrelid::bigint = $1 AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
    )
    .bind(relation)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to load columns for {context}: {error}"))?;
    let spatial_columns: Vec<bool> = column_metadata
        .iter()
        .map(|(_, type_name)| matches!(type_name.as_str(), "geometry" | "geography"))
        .collect();
    let columns: Vec<String> = column_metadata.into_iter().map(|(name, _)| name).collect();
    let postgis_function = if spatial_columns.contains(&true) {
        let schema: Option<String> = sqlx::query_scalar(
            "SELECT n.nspname::text FROM pg_catalog.pg_extension e \
             JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace \
             WHERE e.extname = 'postgis'",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to locate PostGIS for {context}: {error}"))?;
        let schema = schema.ok_or_else(|| {
            format!("PostGIS columns exist for {context}, but the extension is unavailable")
        })?;
        Some(format!(
            "{}.{}",
            quote_identifier(&schema)?,
            quote_identifier("st_asewkt")?
        ))
    } else {
        None
    };

    let edit_metadata = if allow_editing {
        editable_metadata(&mut transaction, relation).await?
    } else {
        None
    };
    let expressions = columns
        .iter()
        .zip(&spatial_columns)
        .map(|(column, spatial)| {
            let column = quote_identifier(column)?;
            // IS NULL on a composite also matches a non-null row of all NULLs.
            // IS NOT DISTINCT FROM NULL tests the datum itself instead.
            let value = if *spatial {
                format!(
                    "{}(t.{column})::text",
                    postgis_function
                        .as_deref()
                        .expect("spatial columns have a PostGIS text function")
                )
            } else {
                format!("pg_catalog.to_json(t.{column})::text")
            };
            Ok(format!(
                "CASE WHEN t.{column} IS NOT DISTINCT FROM NULL THEN NULL::text \
                 ELSE {value} END"
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let selection = if expressions.is_empty() {
        // PostgreSQL permits tables with no columns, including nonempty ones.
        "1::integer".to_owned()
    } else {
        expressions.join(", ")
    };
    let edit_selection = if let Some((edit_columns, keys)) = &edit_metadata {
        let raw_columns = edit_columns
            .iter()
            .zip(&spatial_columns)
            .map(|(column, spatial)| {
                if column.editable_type.is_none() {
                    Ok("NULL::text".to_owned())
                } else if *spatial {
                    Ok(format!(
                        "{}(t.{})::text",
                        postgis_function
                            .as_deref()
                            .expect("spatial columns have a PostGIS text function"),
                        quote_identifier(&column.name)?
                    ))
                } else {
                    Ok(format!("t.{}::text", quote_identifier(&column.name)?))
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        let key_columns = keys
            .iter()
            .map(|key| Ok(format!("t.{}::text", quote_identifier(key)?)))
            .collect::<Result<Vec<_>, String>>()?;
        format!(
            ", t.tableoid::bigint, t.ctid::text, t.xmin::text, {}, {}",
            raw_columns.join(", "),
            key_columns.join(", ")
        )
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT {selection}{edit_selection} FROM {qualified} AS t \
         WHERE pg_catalog.to_regclass($1)::oid::bigint = $2 LIMIT $3"
    );
    let result = sqlx::query(AssertSqlSafe(sql))
        .bind(&qualified)
        .bind(relation)
        .bind(MAX_PREVIEW_ROWS)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to load rows for {context}: {error}"))?;

    // A schema rename can change name resolution without taking the table lock.
    // Do not return an empty preview as though it belonged to the old table.
    let current_relation: Option<i64> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
            .bind(&qualified)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to verify table {context}: {error}"))?;
    if current_relation != Some(relation) {
        return Err(format!("Table {context} changed while loading its preview"));
    }

    let mut rows = Vec::with_capacity(result.len());
    let mut edit_rows = Vec::with_capacity(result.len());
    for (row_index, row) in result.into_iter().enumerate() {
        let expected = columns.len().max(1)
            + edit_metadata
                .as_ref()
                .map_or(0, |(cols, keys)| 3 + cols.len() + keys.len());
        if row.len() != expected {
            return Err(format!(
                "Preview row {} for {context} has {} fields; expected {expected}",
                row_index + 1,
                row.len()
            ));
        }
        let mut values = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            let value: Option<String> = row.try_get(index).map_err(|error| {
                format!(
                    "Failed to decode preview column {column:?} in row {} for {context}: {error}",
                    row_index + 1
                )
            })?;
            let value = if spatial_columns[index] {
                value
            } else {
                format_json_value(value).map_err(|error| {
                    format!(
                        "Failed to convert preview column {column:?} in row {} for {context}: {error}",
                        row_index + 1
                    )
                })?
            };
            values.push(value);
        }
        rows.push(values);
        if let Some((edit_columns, keys)) = &edit_metadata {
            let start = columns.len().max(1);
            let decode_error = |error| {
                format!(
                    "Failed to decode editable row {} for {context}: {error}",
                    row_index + 1
                )
            };
            let table_oid = row.try_get(start).map_err(decode_error)?;
            let ctid = row.try_get(start + 1).map_err(decode_error)?;
            let xmin = row.try_get(start + 2).map_err(decode_error)?;
            let originals = (0..edit_columns.len())
                .map(|ix| row.try_get(start + 3 + ix))
                .collect::<Result<Vec<Option<String>>, _>>()
                .map_err(decode_error)?;
            let key_values = (0..keys.len())
                .map(|ix| row.try_get(start + 3 + edit_columns.len() + ix))
                .collect::<Result<Vec<String>, _>>()
                .map_err(decode_error)?;
            edit_rows.push(EditRow {
                table_oid,
                ctid,
                xmin,
                originals,
                key_values,
            });
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to finish preview for {context}: {error}"))?;
    let edit = edit_metadata.map(|(columns, keys)| EditSnapshot {
        relation,
        columns,
        keys,
        rows: edit_rows,
    });
    Ok((TablePreview { columns, rows }, edit))
}

pub(super) async fn editable_metadata(
    transaction: &mut Transaction<'_, Postgres>,
    relation: i64,
) -> Result<Option<(Vec<EditColumn>, Vec<String>)>, String> {
    let supported: bool = sqlx::query_scalar(
        "SELECT c.relkind = 'r' AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid AND i.indisprimary AND i.indisvalid) FROM pg_catalog.pg_class c WHERE c.oid::bigint = $1"
    ).bind(relation).fetch_optional(&mut **transaction).await.map_err(|error| format!("Could not inspect table identity: {error}"))?.unwrap_or(false);
    if !supported {
        return Ok(None);
    }
    let metadata: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT a.attname::text, n.nspname::text, t.typname::text, (a.attgenerated = '' AND a.attidentity = '') \
         FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE a.attrelid::bigint = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
    ).bind(relation).fetch_all(&mut **transaction).await.map_err(|error| format!("Could not inspect column types: {error}"))?;
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_catalog.pg_index i JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid::bigint = $1 AND i.indisprimary AND i.indisvalid ORDER BY a.attnum"
    ).bind(relation).fetch_all(&mut **transaction).await.map_err(|error| format!("Could not inspect primary key: {error}"))?;
    if keys.is_empty() {
        return Ok(None);
    }
    let columns = metadata
        .into_iter()
        .map(|(name, schema, typ, writable)| {
            let editable_type = writable.then(|| {
                format!(
                    "{}.{}",
                    quote_identifier(&schema)
                        .expect("PostgreSQL catalog schema is a valid identifier"),
                    quote_identifier(&typ).expect("PostgreSQL catalog type is a valid identifier")
                )
            });
            EditColumn {
                name,
                editable_type,
            }
        })
        .collect();
    Ok(Some((columns, keys)))
}

pub(super) fn quote_identifier(identifier: &str) -> Result<String, String> {
    if identifier.is_empty() || identifier.contains('\0') {
        return Err("Identifier must not be empty or contain NUL".into());
    }
    Ok(format!("\"{}\"", identifier.replace('"', "\"\"")))
}

fn format_json_value(value: Option<String>) -> Result<Option<String>, String> {
    match value {
        Some(value) if value.starts_with('"') => serde_json::from_str::<String>(&value)
            .map(Some)
            .map_err(|error| error.to_string()),
        // Keep the server's JSON text for non-strings. Parsing as serde_json::Value
        // would lose numeric precision or reject very large PostgreSQL numerics.
        value => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_individually_quoted_and_reject_invalid_names() {
        assert_eq!(quote_identifier("Mixed.Case").unwrap(), "\"Mixed.Case\"");
        assert_eq!(
            quote_identifier("a\"; SELECT 1; --").unwrap(),
            "\"a\"\"; SELECT 1; --\""
        );
        assert_eq!(quote_identifier("é\n").unwrap(), "\"é\n\"");
        assert!(quote_identifier("").is_err());
        assert!(quote_identifier("name\0other").is_err());
    }

    #[test]
    fn displays_sql_null_strings_and_other_json_without_losing_precision() {
        assert_eq!(format_json_value(None).unwrap(), None);
        assert_eq!(
            format_json_value(Some("null".into())).unwrap(),
            Some("null".into())
        );
        assert_eq!(
            format_json_value(Some(r#""a\n\"b\"\\c""#.into())).unwrap(),
            Some("a\n\"b\"\\c".into())
        );
        assert_eq!(
            format_json_value(Some("\"\"".into())).unwrap(),
            Some(String::new())
        );
        for value in [
            "true",
            "[1,null,\"text\"]",
            "{\"a\":1}",
            "123456789012345678901234567890.123456789",
        ] {
            assert_eq!(
                format_json_value(Some(value.into())).unwrap().as_deref(),
                Some(value)
            );
        }
        assert!(format_json_value(Some("\"unterminated".into())).is_err());
    }
}
