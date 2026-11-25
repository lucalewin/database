use super::table::TablePreview;

/// Results from a script. SQL NULL is represented by `None` in each table.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct QueryOutput {
    pub(crate) statements: Vec<StatementOutput>,
    pub(crate) truncated: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StatementOutput {
    pub(crate) table: Option<TablePreview>,
    pub(crate) rows_affected: Option<u64>,
}
