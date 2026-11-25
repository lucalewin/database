use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use gpui_kit::{
    App, Task, Window,
    component::input::{CompletionProvider, Rope, RopeExt},
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};
use tree_sitter::Parser;

use crate::{database::driver::ColumnMetadata, session::Session};

const SQL_KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "COMMIT",
    "CREATE",
    "CROSS",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "THEN",
    "TRUE",
    "UNION",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
];

#[derive(Default)]
pub(crate) struct SqlCompletionCatalog {
    schemas: Vec<String>,
    tables: Vec<String>,
    columns: Vec<(String, ColumnMetadata)>,
}

impl SqlCompletionCatalog {
    pub(crate) fn from_explorer(state: &Session) -> Self {
        let columns = state.columns().map_or_else(Vec::new, |tables| {
            tables
                .iter()
                .flat_map(|(table, columns)| {
                    columns
                        .iter()
                        .map(move |column| (table.clone(), column.clone()))
                })
                .collect()
        });
        Self {
            schemas: state.schemas().to_vec(),
            tables: state.tables().to_vec(),
            columns,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SqlCompletionScope {
    #[default]
    Any,
    Relation,
    Column,
    Suppress,
}

pub(crate) struct SqlCompletionProvider {
    catalog: Rc<RefCell<SqlCompletionCatalog>>,
    parser: RefCell<Parser>,
}

impl SqlCompletionProvider {
    pub(crate) fn new(catalog: Rc<RefCell<SqlCompletionCatalog>>) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("SQL grammar must load");
        Self {
            catalog,
            parser: RefCell::new(parser),
        }
    }

    fn scope(&self, text: &Rope, offset: usize) -> SqlCompletionScope {
        let qualifier = Self::qualifier(text, offset);
        if qualifier.is_some() {
            return SqlCompletionScope::Column;
        }

        let source = text.to_string();
        let tree = self.parser.borrow_mut().parse(source.as_bytes(), None);
        let Some(tree) = tree else {
            return SqlCompletionScope::Any;
        };
        let position = offset.saturating_sub(1).min(source.len());
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(position, position);
        let mut scope = SqlCompletionScope::Any;
        while let Some(current) = node {
            if matches!(current.kind(), "comment" | "line_comment" | "block_comment") {
                return SqlCompletionScope::Suppress;
            }
            if current.kind().contains("string") || current.kind() == "literal" {
                return SqlCompletionScope::Suppress;
            }
            if matches!(
                current.kind(),
                "select_expression" | "where" | "having" | "group_by" | "order_by"
            ) {
                scope = SqlCompletionScope::Column;
            } else if scope == SqlCompletionScope::Any
                && matches!(
                    current.kind(),
                    "from" | "join" | "relation" | "object_reference"
                )
            {
                scope = SqlCompletionScope::Relation;
            }
            node = current.parent();
        }
        scope
    }

    fn table_references(text: &Rope, offset: usize) -> BTreeMap<String, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("SQL grammar must load");
        let source = text.to_string();
        let Some(tree) = parser.parse(source.as_bytes(), None) else {
            return BTreeMap::new();
        };
        let mut references = BTreeMap::new();
        let mut cursor = tree.root_node().walk();
        for node in tree.root_node().children(&mut cursor) {
            Self::collect_table_references(node, &source, offset, &mut references);
        }
        references
    }

    fn collect_table_references(
        node: tree_sitter::Node,
        source: &str,
        offset: usize,
        references: &mut BTreeMap<String, String>,
    ) {
        if node.start_byte() > offset {
            return;
        }
        if node.kind() == "relation"
            && let Some(reference) = node.child_by_field_name("name")
        {
            let table = reference
                .utf8_text(source.as_bytes())
                .expect("tree-sitter byte range is UTF-8");
            if let Some(alias) = node.child_by_field_name("alias") {
                let alias = alias
                    .utf8_text(source.as_bytes())
                    .expect("tree-sitter byte range is UTF-8");
                references.insert(alias.to_lowercase(), table.to_owned());
            }
            references.insert(table.to_lowercase(), table.to_owned());
        }
        let Some(object) = (node.kind() == "relation")
            .then(|| node.named_child(0))
            .flatten()
            .filter(|child| child.kind() == "object_reference")
        else {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::collect_table_references(child, source, offset, references);
            }
            return;
        };
        if let Some(name) = object.child_by_field_name("name") {
            let table = name
                .utf8_text(source.as_bytes())
                .expect("tree-sitter byte range is UTF-8");
            references.insert(table.to_lowercase(), table.to_owned());
            if let Some(alias) = node.child_by_field_name("alias") {
                let alias = alias
                    .utf8_text(source.as_bytes())
                    .expect("tree-sitter byte range is UTF-8");
                references.insert(alias.to_lowercase(), table.to_owned());
            }
        }
    }

    fn prefix(text: &Rope, offset: usize) -> String {
        let prefix = text.slice(..offset).to_string();
        prefix
            .trim_start_matches(|character: char| !character.is_alphanumeric() && character != '_')
            .rsplit(|character: char| !character.is_alphanumeric() && character != '_')
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    fn match_rank(label: &str, prefix: &str) -> Option<(u8, usize)> {
        if prefix.is_empty() {
            return Some((4, label.len()));
        }
        let label = label.to_lowercase();
        let prefix = prefix.to_lowercase();
        if label == prefix {
            Some((0, label.len()))
        } else if label.starts_with(&prefix) {
            Some((1, label.len()))
        } else if label.contains(&prefix) {
            Some((2, label.len()))
        } else {
            let mut rest = label.as_str();
            for character in prefix.chars() {
                let position = rest.find(character)?;
                rest = &rest[position + character.len_utf8()..];
            }
            Some((3, label.len()))
        }
    }

    fn completion_items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let scope = self.scope(text, offset);
        if scope == SqlCompletionScope::Suppress {
            return Vec::new();
        }

        let prefix = Self::prefix(text, offset);
        let catalog = self.catalog.borrow();
        let qualifier = Self::qualifier(text, offset);
        let references = Self::table_references(text, offset);
        let referenced_tables = references.values().cloned().collect::<BTreeSet<_>>();
        let mut items = Vec::new();
        if scope != SqlCompletionScope::Relation {
            let relation = qualifier
                .as_deref()
                .and_then(|qualifier| references.get(&qualifier.to_lowercase()))
                .map(String::as_str)
                .or(qualifier.as_deref());
            items.extend(Self::column_items(&catalog, relation, &referenced_tables));
        }
        if qualifier.is_none() {
            if scope != SqlCompletionScope::Column {
                items.extend(SQL_KEYWORDS.iter().map(|keyword| {
                    Self::item(
                        (*keyword).to_owned(),
                        CompletionItemKind::KEYWORD,
                        "SQL keyword",
                    )
                }));
                items.extend(catalog.schemas.iter().map(|schema| {
                    let mut item = Self::item(schema, CompletionItemKind::MODULE, "Schema");
                    item.insert_text = Some(Self::quote_identifier(schema));
                    item
                }));
            }
            items.extend(catalog.tables.iter().map(|table| {
                let mut item = Self::item(table, CompletionItemKind::CLASS, "Table");
                item.insert_text = Some(Self::quote_identifier(table));
                item
            }));
            if catalog
                .columns
                .iter()
                .any(|(_, column)| column.is_spatial())
                && scope != SqlCompletionScope::Relation
            {
                for function in [
                    "ST_AsGeoJSON",
                    "ST_AsText",
                    "ST_Transform",
                    "ST_Intersects",
                    "ST_DWithin",
                    "ST_SRID",
                    "ST_X",
                    "ST_Y",
                ] {
                    let mut item =
                        Self::item(function, CompletionItemKind::FUNCTION, "PostGIS function");
                    item.insert_text = Some(format!("{function}("));
                    items.push(item);
                }
            }
        }

        let context_priority = |kind: Option<CompletionItemKind>| match (scope, kind) {
            (SqlCompletionScope::Relation, Some(CompletionItemKind::CLASS)) => 0,
            (SqlCompletionScope::Column, Some(CompletionItemKind::FIELD)) => 0,
            (_, Some(CompletionItemKind::CLASS)) => 1,
            (_, Some(CompletionItemKind::FIELD)) => 2,
            (_, Some(CompletionItemKind::KEYWORD)) => 1,
            (_, Some(CompletionItemKind::MODULE)) => 3,
            _ => 4,
        };
        let mut ranked = items
            .into_iter()
            .filter_map(|item| {
                let rank = Self::match_rank(&item.label, &prefix)?;
                Some((rank, context_priority(item.kind), item))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| {
                    left.2
                        .label
                        .to_lowercase()
                        .cmp(&right.2.label.to_lowercase())
                })
        });

        let mut seen = BTreeSet::new();
        let mut items = Vec::with_capacity(ranked.len());
        for (_, _, mut item) in ranked {
            if !seen.insert((item.label.clone(), item.detail.clone())) {
                continue;
            }
            item.filter_text = Some(prefix.clone());
            Self::apply_text_edit(&mut item, text, offset, prefix.len());
            if items.is_empty() {
                item.preselect = Some(true);
            }
            items.push(item);
        }
        items
    }

    fn apply_text_edit(item: &mut CompletionItem, text: &Rope, offset: usize, prefix_len: usize) {
        let start = offset.saturating_sub(prefix_len);
        let insert_text = item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone());
        let trailing_space = if matches!(
            item.kind,
            Some(CompletionItemKind::KEYWORD | CompletionItemKind::CLASS)
        ) && text
            .slice(offset..)
            .chars()
            .next()
            .is_none_or(|character| !character.is_whitespace())
        {
            " "
        } else {
            ""
        };
        item.text_edit = Some(CompletionTextEdit::Edit(TextEdit {
            range: Range::new(
                text.offset_to_position(start),
                text.offset_to_position(offset),
            ),
            new_text: format!("{insert_text}{trailing_space}"),
        }));
    }

    fn qualifier(text: &Rope, offset: usize) -> Option<String> {
        let prefix = text.slice(..offset).to_string();
        let before_word = prefix.trim_end_matches(|character: char| {
            character.is_alphanumeric() || character == '_' || character == '$'
        });
        let before_dot = before_word.strip_suffix('.')?;
        let qualifier = before_dot
            .rsplit(|character: char| {
                !(character.is_alphanumeric() || character == '_' || character == '$')
            })
            .next()?
            .to_owned();
        (!qualifier.is_empty()).then_some(qualifier)
    }

    fn item(label: impl Into<String>, kind: CompletionItemKind, detail: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            kind: Some(kind),
            detail: Some(detail.to_owned()),
            ..CompletionItem::default()
        }
    }

    fn quote_identifier(name: &str) -> String {
        let mut characters = name.chars();
        let simple = characters
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
            && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
            && !name.chars().any(|character| character.is_ascii_uppercase());
        if simple {
            name.to_owned()
        } else {
            format!("\"{}\"", name.replace('"', "\"\""))
        }
    }

    fn column_items(
        catalog: &SqlCompletionCatalog,
        qualifier: Option<&str>,
        referenced_tables: &BTreeSet<String>,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        for (table, column) in &catalog.columns {
            let in_scope = qualifier.map_or(
                referenced_tables.is_empty() || referenced_tables.contains(table),
                |qualifier| qualifier == table,
            );
            if !in_scope {
                continue;
            }
            let mut item = Self::item(&column.name, CompletionItemKind::FIELD, table);
            item.detail = Some(format!("{table} · {}", column.detail()));
            item.documentation = column
                .description
                .clone()
                .map(lsp_types::Documentation::String);
            item.insert_text = Some(Self::quote_identifier(&column.name));
            items.push(item);
        }
        items
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(
            self.completion_items(text, offset),
        )))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().last().is_some_and(|character| {
            character.is_alphanumeric() || character == '_' || character == '.'
        })
    }
}

#[cfg(test)]
mod tests;
