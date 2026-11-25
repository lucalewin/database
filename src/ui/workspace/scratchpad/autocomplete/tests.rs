use crate::ui::workspace::scratchpad::autocomplete::*;

fn test_column(table: &str, name: &str) -> ColumnMetadata {
    ColumnMetadata {
        name: name.into(),
        data_type: if table == "products" {
            "text"
        } else {
            "bigint"
        }
        .into(),
        udt_name: if table == "products" { "text" } else { "int8" }.into(),
        nullable: true,
        has_default: false,
        identity: None,
        generated: None,
        primary_key: name == "id",
        foreign_key: false,
        description: None,
        postgis_type: None,
        postgis_srid: None,
        postgis_dimensions: None,
    }
}

#[test]
fn completion_qualifier_uses_the_identifier_before_the_dot() {
    let text = Rope::from_str("SELECT orders.i FROM orders");
    assert_eq!(
        SqlCompletionProvider::qualifier(&text, "SELECT orders.i".len()).as_deref(),
        Some("orders")
    );
    assert_eq!(
        SqlCompletionProvider::qualifier(&Rope::from_str("SELECT ord"), "SELECT ord".len()),
        None
    );
}

#[test]
fn completion_ranking_puts_product_prefix_matches_first() {
    assert_eq!(
        SqlCompletionProvider::match_rank("products", "pro"),
        Some((1, "products".len()))
    );
    assert_eq!(
        SqlCompletionProvider::match_rank("product_sales", "ps"),
        Some((3, "product_sales".len()))
    );
    assert_eq!(SqlCompletionProvider::match_rank("customers", "pro"), None);
}

#[test]
fn completion_scope_uses_sql_tree_sitter() {
    let provider =
        SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog::default())));
    let relation = Rope::from_str("SELECT * FROM pro");
    assert_eq!(
        provider.scope(&relation, relation.to_string().len()),
        SqlCompletionScope::Relation
    );
    let column = Rope::from_str("SELECT products.na");
    assert_eq!(
        provider.scope(&column, column.to_string().len()),
        SqlCompletionScope::Column
    );
    let comment = Rope::from_str("-- type pro");
    assert_eq!(
        provider.scope(&comment, comment.to_string().len()),
        SqlCompletionScope::Suppress
    );
}

#[test]
fn completion_query_filters_and_prioritizes_products() {
    let provider = SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["orders".into(), "products".into(), "product_sales".into()],
        columns: vec![("orders".into(), test_column("orders", "product_id"))],
    })));
    let items = provider.completion_items(&Rope::from_str("SELECT * FROM pro"), 17);
    assert_eq!(
        items.first().map(|item| item.label.as_str()),
        Some("products")
    );
    assert_eq!(
        items.first().and_then(|item| item.text_edit.as_ref()),
        Some(&CompletionTextEdit::Edit(TextEdit {
            range: Range::new(
                lsp_types::Position::new(0, 14),
                lsp_types::Position::new(0, 17),
            ),
            new_text: "products ".into(),
        }))
    );
    assert!(
        items
            .iter()
            .all(|item| item.label.to_lowercase().contains("pro"))
    );
}

#[test]
fn completion_where_only_suggests_columns_from_referenced_tables() {
    let provider = SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["orders".into(), "products".into()],
        columns: vec![
            ("orders".into(), test_column("orders", "customer_id")),
            ("products".into(), test_column("products", "price")),
            ("products".into(), test_column("products", "in_stock")),
        ],
    })));
    let text = Rope::from_str("SELECT * FROM products WHERE ");
    let items = provider.completion_items(&text, text.to_string().len());
    assert!(items.iter().all(|item| {
        item.kind != Some(CompletionItemKind::FIELD)
            || item
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("products ·"))
    }));
    assert!(items.iter().any(|item| item.label == "price"));
    assert!(!items.iter().any(|item| item.label == "customer_id"));
}

#[test]
fn completion_qualified_alias_only_suggests_its_columns() {
    let provider = SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["orders".into(), "products".into()],
        columns: vec![
            ("orders".into(), test_column("orders", "quantity")),
            ("products".into(), test_column("products", "price")),
        ],
    })));
    let text = Rope::from_str("SELECT * FROM products AS p WHERE p.");
    let items = provider.completion_items(&text, text.to_string().len());
    assert_eq!(
        items
            .iter()
            .filter(|item| item.kind == Some(CompletionItemKind::FIELD))
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["price"]
    );
}

#[test]
fn completion_edits_replace_the_prefix_and_follow_columns_without_space() {
    let provider = SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["products".into()],
        columns: vec![("products".into(), test_column("products", "name"))],
    })));
    let keyword = provider.completion_items(&Rope::from_str("sel"), 3)[0].clone();
    assert_eq!(
        keyword.text_edit,
        Some(CompletionTextEdit::Edit(TextEdit {
            range: Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, 3),
            ),
            new_text: "SELECT ".into(),
        }))
    );
    let text = Rope::from_str("SELECT products.na");
    let column = provider
        .completion_items(&text, text.to_string().len())
        .into_iter()
        .find(|item| item.label == "name")
        .expect("name completion");
    assert_eq!(
        column.text_edit,
        Some(CompletionTextEdit::Edit(TextEdit {
            range: Range::new(
                lsp_types::Position::new(0, 16),
                lsp_types::Position::new(0, 18),
            ),
            new_text: "name".into(),
        }))
    );
}

#[test]
fn completion_columns_are_scoped_to_a_qualified_table() {
    let catalog = SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["orders".into(), "users".into()],
        columns: vec![
            ("orders".into(), test_column("orders", "id")),
            ("orders".into(), test_column("orders", "total")),
            ("users".into(), test_column("users", "email")),
        ],
    };
    let items = SqlCompletionProvider::column_items(&catalog, Some("orders"), &BTreeSet::new());
    assert_eq!(
        items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["id", "total"]
    );
    assert!(items.iter().all(|item| {
        item.detail
            .as_deref()
            .is_some_and(|detail| detail.starts_with("orders ·"))
    }));
}

#[test]
fn spatial_metadata_drives_detail_and_function_suggestions() {
    let mut geometry = test_column("locations", "geom");
    geometry.udt_name = "geometry".into();
    geometry.data_type = "geometry(Point,4326)".into();
    geometry.postgis_type = Some("POINT".into());
    geometry.postgis_srid = Some(4326);
    geometry.postgis_dimensions = Some(2);
    let provider = SqlCompletionProvider::new(Rc::new(RefCell::new(SqlCompletionCatalog {
        schemas: vec![],
        tables: vec!["locations".into()],
        columns: vec![("locations".into(), geometry)],
    })));
    let text = Rope::from_str("SELECT * FROM locations WHERE ST_");
    let items = provider.completion_items(&text, text.to_string().len());
    assert!(items.iter().any(|item| item.label == "ST_AsGeoJSON"));
    let geom = SqlCompletionProvider::column_items(
        &provider.catalog.borrow(),
        Some("locations"),
        &BTreeSet::new(),
    )
    .into_iter()
    .find(|item| item.label == "geom")
    .expect("geometry column");
    assert!(
        geom.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("POINT, SRID 4326, 2D"))
    );
}

#[test]
fn completion_identifiers_are_quoted_only_when_required() {
    assert_eq!(SqlCompletionProvider::quote_identifier("orders"), "orders");
    assert_eq!(
        SqlCompletionProvider::quote_identifier("Order Items"),
        "\"Order Items\""
    );
    assert_eq!(
        SqlCompletionProvider::quote_identifier("say\"hello"),
        "\"say\"\"hello\""
    );
}
